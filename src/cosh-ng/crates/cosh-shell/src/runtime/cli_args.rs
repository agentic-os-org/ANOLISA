use crate::runtime::prelude::CoshConfig;

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
    pub(crate) initial_question: Option<String>,
    pub(crate) failure_context: Option<String>,
    pub(crate) login: bool,
    pub(crate) isolated: bool,
    pub(crate) trusted_login_bash: Option<String>,
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

pub(crate) fn configured_raw_invocation(
    parsed: super::invocation::TuiEntry,
    config: &CoshConfig,
) -> (String, RawShellKind, LaunchOptions) {
    let adapter_name = parsed
        .adapter_name
        .unwrap_or_else(|| config.adapter_default.clone());
    let shell_kind = parsed.shell_kind.unwrap_or_else(|| {
        std::env::var("COSH_SHELL_RAW_SHELL")
            .ok()
            .map(|shell| parse_raw_shell(&shell))
            .unwrap_or_else(|| shell_from_default_or_auto(&config.shell_default))
    });
    let launch_options = parsed.launch_options;
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
        cosh_shell_default_state_path_for_home, parse_raw_shell, shell_from_default_or_auto,
        LaunchOptions, RawShellKind, ResumeLaunch,
    };

    fn parse_raw_invocation(args: &[String]) -> super::super::invocation::TuiEntry {
        use super::super::invocation::{classify_invocation, Invocation};
        use std::ffi::{OsStr, OsString};
        let mut args_os = vec![OsString::from("raw")];
        args_os.extend(args.iter().map(OsString::from));
        match classify_invocation(OsStr::new("cosh-shell"), &args_os, true, true, true) {
            Invocation::Tui(entry) => entry,
            other => panic!("expected Tui, got {other:?}"),
        }
    }

    fn adapter_name_from_args(args: &[String]) -> Option<String> {
        parse_raw_invocation(args).adapter_name
    }

    fn raw_shell_from_args(args: &[String]) -> Option<RawShellKind> {
        parse_raw_invocation(args).shell_kind
    }

    fn launch_options_from_args(args: &[String]) -> LaunchOptions {
        parse_raw_invocation(args).launch_options
    }

    #[test]
    fn raw_launch_collects_options_in_one_result() {
        let args = [
            "qwen",
            "--shell",
            "zsh",
            "--resume",
            "session-1",
            "--login",
            "--isolated",
        ]
        .map(str::to_string);
        let parsed = parse_raw_invocation(&args);
        assert_eq!(parsed.adapter_name, Some("qwen".to_string()));
        assert_eq!(parsed.shell_kind, Some(RawShellKind::Zsh));
        assert_eq!(
            parsed.launch_options.resume,
            Some(ResumeLaunch::Session("session-1".into()))
        );
        assert!(parsed.launch_options.login);
        assert!(parsed.launch_options.isolated);
    }

    #[test]
    fn raw_launch_consumes_shell_value_before_startup_flags() {
        let args = ["--shell", "--isolated"].map(str::to_string);
        let parsed = parse_raw_invocation(&args);
        assert_eq!(parsed.shell_kind, Some(RawShellKind::MissingShellValue));
        assert!(!parsed.launch_options.isolated);
    }

    #[test]
    fn raw_launch_repeated_shell_values_are_not_startup_flags() {
        for value in ["-l", "--login", "--isolated"] {
            let args = ["fake", "--shell", "bash", "--shell", value].map(str::to_string);
            let parsed = parse_raw_invocation(&args);
            assert_eq!(parsed.shell_kind, Some(RawShellKind::Bash));
            assert!(!parsed.launch_options.login);
            assert!(!parsed.launch_options.isolated);
        }
    }

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
            Some("qwen".to_string())
        );
        assert_eq!(
            adapter_name_from_args(&["--shell".to_string(), "zsh".to_string(), "co".to_string()]),
            Some("co".to_string())
        );
        assert_eq!(
            adapter_name_from_args(&[
                "cosh-core".to_string(),
                "--resume".to_string(),
                "00000000-0000-4000-8000-000000000000".to_string(),
                "--shell".to_string(),
                "zsh".to_string(),
            ]),
            Some("cosh-core".to_string())
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
