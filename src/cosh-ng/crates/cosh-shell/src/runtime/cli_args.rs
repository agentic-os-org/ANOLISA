use crate::runtime::prelude::load_config;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RawShellKind {
    Bash,
    Zsh,
    MissingShellValue,
    Unsupported(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResumeLaunch {
    Picker,
    Session(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LaunchOptions {
    pub(crate) resume: Option<ResumeLaunch>,
}

pub(crate) fn adapter_name_from_args(args: &[String]) -> Option<&str> {
    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--shell" => idx += 2,
            "--resume" => {
                idx += 1;
                if args.get(idx).is_some_and(|value| !value.starts_with('-')) {
                    idx += 1;
                }
            }
            // Standalone allowlist flags share one vocabulary source with the
            // classifier, so a token admitted into the TUI there can never be
            // read as an adapter name here.
            arg if crate::runtime::invocation::TUI_STANDALONE_FLAGS.contains(&arg) => idx += 1,
            arg if arg.starts_with("--shell=") => idx += 1,
            arg if arg.starts_with("--resume=") => idx += 1,
            arg if arg.starts_with("--") => idx += 1,
            arg => return Some(arg),
        }
    }

    None
}

pub(crate) fn raw_shell_from_args_or_default(args: &[String], default_shell: &str) -> RawShellKind {
    if let Some(shell) = raw_shell_from_args(args) {
        return shell;
    }

    if let Some(shell) = std::env::var("COSH_SHELL_RAW_SHELL")
        .ok()
        .as_deref()
        .map(parse_raw_shell)
    {
        return shell;
    }

    shell_from_default_or_auto(default_shell)
}

pub(crate) fn raw_shell_from_args(args: &[String]) -> Option<RawShellKind> {
    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--shell" => {
                return Some(match args.get(idx + 1) {
                    Some(value) if !value.starts_with("--") => parse_raw_shell(value),
                    _ => RawShellKind::MissingShellValue,
                });
            }
            arg if arg.starts_with("--shell=") => {
                return Some(parse_raw_shell(arg.trim_start_matches("--shell=")));
            }
            _ => idx += 1,
        }
    }

    None
}

pub(crate) fn parse_raw_shell(value: &str) -> RawShellKind {
    let name = value.rsplit('/').next().unwrap_or(value);
    match name {
        "bash" | "cosh-shell-bash" => RawShellKind::Bash,
        "zsh" | "cosh-shell-zsh" => RawShellKind::Zsh,
        other => RawShellKind::Unsupported(other.to_string()),
    }
}

pub(crate) fn shell_from_default_or_auto(value: &str) -> RawShellKind {
    let value = value.trim();
    if !value.is_empty() && value != "auto" {
        return parse_raw_shell(value);
    }

    for candidate in [
        cosh_shell_default_state_previous_shell(),
        std::env::var("SHELL").ok(),
    ]
    .into_iter()
    .flatten()
    {
        let shell = parse_raw_shell(&candidate);
        if matches!(shell, RawShellKind::Bash | RawShellKind::Zsh) {
            return shell;
        }
    }

    RawShellKind::Bash
}

pub(crate) fn launch_options_from_args(args: &[String]) -> LaunchOptions {
    let mut options = LaunchOptions::default();
    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--shell" => idx += 2,
            "--resume" => {
                options.resume = match args.get(idx + 1) {
                    Some(value) if !value.starts_with('-') => {
                        idx += 2;
                        Some(ResumeLaunch::Session(value.clone()))
                    }
                    _ => {
                        idx += 1;
                        Some(ResumeLaunch::Picker)
                    }
                };
            }
            arg if arg.starts_with("--resume=") => {
                let value = arg.trim_start_matches("--resume=");
                options.resume = Some(if value.is_empty() {
                    ResumeLaunch::Picker
                } else {
                    ResumeLaunch::Session(value.to_string())
                });
                idx += 1;
            }
            _ => idx += 1,
        }
    }
    options
}

pub(crate) fn configured_raw_invocation(args: &[String]) -> (String, RawShellKind, LaunchOptions) {
    let config = load_config();
    let adapter_name = adapter_name_from_args(args)
        .unwrap_or(&config.adapter_default)
        .to_string();
    let shell_kind = raw_shell_from_args_or_default(args, &config.shell_default);
    let launch_options = launch_options_from_args(args);
    (adapter_name, shell_kind, launch_options)
}

fn cosh_shell_default_state_previous_shell() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let path = cosh_shell_default_state_path_for_home(std::path::Path::new(&home));
    let content = std::fs::read_to_string(path).ok()?;
    content.lines().find_map(|line| {
        line.strip_prefix("PREVIOUS_SHELL=")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn cosh_shell_default_state_path_for_home(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".copilot-shell/cosh/cosh-shell-default.state")
}

#[cfg(test)]
mod tests {
    use super::{
        adapter_name_from_args, cosh_shell_default_state_path_for_home, launch_options_from_args,
        parse_raw_shell, raw_shell_from_args, shell_from_default_or_auto, RawShellKind,
        ResumeLaunch,
    };

    #[test]
    fn raw_shell_selection_uses_explicit_arg_only() {
        assert_eq!(parse_raw_shell("/bin/zsh"), RawShellKind::Zsh);
        assert_eq!(parse_raw_shell("bash"), RawShellKind::Bash);
        assert_eq!(
            parse_raw_shell("/usr/local/bin/cosh-shell-zsh"),
            RawShellKind::Zsh
        );
        assert_eq!(
            parse_raw_shell("/usr/local/bin/cosh-shell-bash"),
            RawShellKind::Bash
        );
        assert_eq!(
            parse_raw_shell("/usr/bin/fish"),
            RawShellKind::Unsupported("fish".to_string())
        );
        assert_eq!(
            raw_shell_from_args(&["fake".to_string(), "--shell".to_string(), "zsh".to_string()]),
            Some(RawShellKind::Zsh)
        );
        assert_eq!(
            raw_shell_from_args(&[
                "fake".to_string(),
                "--shell=bash".to_string(),
                "--run".to_string()
            ]),
            Some(RawShellKind::Bash)
        );
        assert_eq!(
            raw_shell_from_args(&["fake".to_string(), "--run".to_string()]),
            None
        );
        assert_eq!(
            raw_shell_from_args(&["fake".to_string(), "--shell".to_string()]),
            Some(RawShellKind::MissingShellValue)
        );
        assert_eq!(
            raw_shell_from_args(&[
                "fake".to_string(),
                "--shell".to_string(),
                "--run".to_string()
            ]),
            Some(RawShellKind::MissingShellValue)
        );
        assert_eq!(
            adapter_name_from_args(&["--shell".to_string(), "zsh".to_string(), "qwen".to_string()]),
            Some("qwen")
        );
        assert_eq!(
            adapter_name_from_args(&["--shell".to_string(), "zsh".to_string(), "co".to_string()]),
            Some("co")
        );
        assert_eq!(
            adapter_name_from_args(&[
                "cosh-core".to_string(),
                "--resume".to_string(),
                "00000000-0000-4000-8000-000000000000".to_string(),
                "--shell".to_string(),
                "zsh".to_string(),
            ]),
            Some("cosh-core")
        );
        assert_eq!(
            adapter_name_from_args(&[
                "--resume".to_string(),
                "00000000-0000-4000-8000-000000000000".to_string(),
                "--shell=zsh".to_string(),
            ]),
            None
        );
    }

    #[test]
    fn default_shell_state_path_uses_copilot_shell_cosh_dir() {
        assert_eq!(
            cosh_shell_default_state_path_for_home(std::path::Path::new("/tmp/cosh-home")),
            std::path::PathBuf::from("/tmp/cosh-home/.copilot-shell/cosh/cosh-shell-default.state")
        );
    }

    #[test]
    fn raw_shell_default_uses_config_before_auto() {
        assert_eq!(shell_from_default_or_auto("zsh"), RawShellKind::Zsh);
        assert_eq!(shell_from_default_or_auto("/bin/bash"), RawShellKind::Bash);
        assert_eq!(
            shell_from_default_or_auto("/usr/bin/fish"),
            RawShellKind::Unsupported("fish".to_string())
        );
    }

    #[test]
    fn tui_allowlist_shapes_never_parse_an_adapter_name() {
        // Vocabulary contract shared with the invocation classifier. The
        // standalone-flag shapes are generated from the classifier's own
        // vocabulary (`TUI_STANDALONE_FLAGS`), so a flag added there is
        // covered here mechanically; only the value-carrying shapes
        // (`--shell`, `--resume`) remain hand-enumerated because their
        // shapes are flag-specific.
        use crate::runtime::invocation::{classify_invocation, Invocation, TUI_STANDALONE_FLAGS};
        use std::ffi::{OsStr, OsString};

        let mut shapes: Vec<Vec<String>> = vec![vec![]];
        for flag in TUI_STANDALONE_FLAGS {
            shapes.push(vec![flag.to_string()]);
        }
        // All standalone flags combined, and each behind a consumed --shell.
        shapes.push(TUI_STANDALONE_FLAGS.iter().map(|s| s.to_string()).collect());
        for flag in TUI_STANDALONE_FLAGS {
            shapes.push(vec!["--shell".into(), "zsh".into(), flag.to_string()]);
        }
        for value_shape in [
            vec!["--resume".to_string()],
            vec![
                "--resume".to_string(),
                "00000000-0000-4000-8000-000000000000".to_string(),
            ],
            vec!["--shell=bash".to_string()],
        ] {
            shapes.push(value_shape);
        }

        for args in shapes {
            let os_args: Vec<OsString> = args.iter().map(OsString::from).collect();
            assert!(
                matches!(
                    classify_invocation(OsStr::new("cosh"), &os_args, true, true, true),
                    Invocation::Tui(_)
                ),
                "allowlist shape must stay TUI: {args:?}"
            );
            assert_eq!(
                adapter_name_from_args(&args),
                None,
                "allowlist token misread as adapter: {args:?}"
            );
        }
    }

    #[test]
    fn resume_launch_parsing_never_treats_option_values_as_adapters() {
        assert_eq!(
            launch_options_from_args(&["--resume".to_string()]).resume,
            Some(ResumeLaunch::Picker)
        );
        assert_eq!(
            launch_options_from_args(&[
                "cosh-core".to_string(),
                "--shell".to_string(),
                "zsh".to_string(),
                "--resume".to_string(),
                "00000000-0000-4000-8000-000000000000".to_string(),
            ])
            .resume,
            Some(ResumeLaunch::Session(
                "00000000-0000-4000-8000-000000000000".to_string()
            ))
        );
        assert_eq!(
            launch_options_from_args(&["--resume=".to_string()]).resume,
            Some(ResumeLaunch::Picker)
        );
    }
}
