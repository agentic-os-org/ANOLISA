//! Disambiguate package installation and legacy native integration before any side effect.

use crate::cli::Result;
use std::ffi::OsString;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Route<'a> {
    Help,
    Runtime {
        command: &'a str,
        arguments: &'a [String],
    },
    Package {
        command: &'a str,
        arguments: &'a [String],
    },
}

pub(crate) fn decode(arguments: impl Iterator<Item = OsString>) -> Result<Vec<String>> {
    arguments
        .map(|argument| {
            argument
                .into_string()
                .map_err(|_| "command arguments must be valid UTF-8".into())
        })
        .collect()
}

pub(crate) fn select(arguments: &[String]) -> Result<Route<'_>> {
    let (command, arguments) = arguments.split_first().ok_or(
        "expected run, install, uninstall, agent, config, inspect, policy, serve, status, request, stop",
    )?;
    match command.as_str() {
        "--help" => Ok(Route::Help),
        "agent" => {
            let (subcommand, arguments) =
                arguments.split_first().ok_or("expected agent connect")?;
            if subcommand != "connect" {
                return Err("unknown agent command; expected connect".into());
            }
            Ok(Route::Runtime {
                command: "install",
                arguments,
            })
        }
        "config" if arguments.first().is_some_and(|value| value == "validate") => {
            Ok(Route::Runtime {
                command: "validate",
                arguments: &arguments[1..],
            })
        }
        "install" => {
            let (mut native, mut package) = (false, false);
            let mut options = arguments.iter();
            while let Some(option) = options.next() {
                if option == "--" {
                    break;
                }
                native |= matches!(option.as_str(), "--config" | "--agent" | "--native-profile");
                package |= matches!(option.as_str(), "--prefix" | "--bundle");
                // Install options are value-bearing; literal values are not command selectors.
                options.next();
            }
            if native && package {
                return Err("cannot combine software installation options (--prefix/--bundle) with native Agent integration (--config/--agent/--native-profile); use aw agent connect for native integration".into());
            }
            if native {
                Ok(Route::Runtime { command, arguments })
            } else {
                Ok(Route::Package { command, arguments })
            }
        }
        "uninstall" | "config" => Ok(Route::Package { command, arguments }),
        _ => Ok(Route::Runtime { command, arguments }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(input: &str) -> Vec<String> {
        input.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn native_install_and_connect_share_exact_runtime_arguments() {
        let old = words("install --config local.yaml --agent hermes --native-profile work");
        let new = words("agent connect --config local.yaml --agent hermes --native-profile work");
        assert_eq!(select(&old).unwrap(), select(&new).unwrap());
    }

    #[test]
    fn configuration_validation_keeps_legacy_scope_comparison() {
        let old = words("validate --config candidate.yaml --against original.yaml");
        let new = words("config validate --config candidate.yaml --against original.yaml");
        assert_eq!(select(&old).unwrap(), select(&new).unwrap());
    }

    #[test]
    fn package_commands_share_one_frontend_without_native_dispatch() {
        for input in [
            "install --prefix /private/prefix --bundle /private/bundle",
            "uninstall --prefix /private/prefix",
            "config init --prefix /private/prefix --config /private/aw.yaml",
            "config add-provider --config /private/aw.yaml --name external --definition provider.yaml",
        ] {
            let arguments = words(input);
            assert!(matches!(select(&arguments).unwrap(), Route::Package { .. }));
        }
    }

    #[test]
    fn mixed_installation_options_are_rejected_before_dispatch() {
        for input in [
            "install --prefix /private/prefix --config local.yaml",
            "install --agent hermes --bundle /private/bundle",
            "install --native-profile work --prefix /private/prefix",
        ] {
            assert!(select(&words(input))
                .unwrap_err()
                .to_string()
                .contains("cannot combine"));
        }
    }

    #[test]
    fn unsupported_agent_commands_do_not_fall_through_to_installation() {
        for input in ["agent", "agent install", "agent disconnect"] {
            assert!(select(&words(input)).is_err());
        }
    }

    #[test]
    fn option_values_do_not_select_another_installation_mode() {
        let native = words("install --config --prefix --agent target");
        assert!(matches!(select(&native).unwrap(), Route::Runtime { .. }));
        let package = words("install --prefix --config --bundle /private/bundle");
        assert!(matches!(select(&package).unwrap(), Route::Package { .. }));
    }

    #[test]
    fn run_forwards_native_arguments_without_reinterpreting_them() {
        let input = words("run --config aw.yaml --agent qoder -- --prefix --bundle");
        assert_eq!(
            select(&input).unwrap(),
            Route::Runtime {
                command: "run",
                arguments: &input[1..],
            }
        );
    }

    #[test]
    fn invalid_utf8_returns_an_error_without_panicking() {
        use std::os::unix::ffi::OsStringExt;
        let result = decode([OsString::from_vec(vec![0xff])].into_iter());
        assert!(result.unwrap_err().to_string().contains("UTF-8"));
    }
}
