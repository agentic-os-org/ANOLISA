//! Retain routing invariants while testing the typed parser without side effects.

use super::*;

fn parse(input: &str) -> Result<Command, clap::Error> {
    Cli::try_parse_from(std::iter::once("aw").chain(input.split_whitespace()))
        .map(|cli| cli.command)
}

#[test]
fn command_declaration_is_internally_consistent() {
    use clap::CommandFactory;
    Cli::command().debug_assert();
}

#[test]
fn obsolete_public_spellings_are_rejected() {
    for old in [
        "install --config local.yaml --agent target",
        "agent connect --config local.yaml --agent target",
        "instance update --instance one",
        "instance attach --agent target --pid 123",
        "instance attach --sdk-instance one",
        "instance detach --sdk-instance one",
        "serve --config aw.yaml --state-dir /private/state",
        "status --config aw.yaml",
        "stop --config aw.yaml",
        "validate --config aw.yaml",
    ] {
        assert!(parse(old).is_err(), "{old}");
    }
}

#[test]
fn configuration_validation_preserves_scope_and_output_options() {
    let Command::Config(Config::Validate(value)) =
        parse("config validate --config candidate.yaml --against original.yaml --json").unwrap()
    else {
        panic!("expected config validation")
    };
    assert_eq!(value.config, "candidate.yaml");
    assert_eq!(value.against.as_deref(), Some("original.yaml"));
    assert!(value.output.json);
}

#[test]
fn service_groups_preserve_runtime_arguments() {
    let Command::Service(Service::Serve(value)) =
        parse("service serve --config aw.yaml --state-dir /private/state").unwrap()
    else {
        panic!("expected serve")
    };
    assert_eq!(value.config, "aw.yaml");
    assert_eq!(value.state_dir, "/private/state");
    for action in ["status", "stop"] {
        for (selection, config, socket) in [
            ("--config aw.yaml", Some("aw.yaml"), None),
            ("--socket /socket", None, Some("/socket")),
        ] {
            let Command::Service(Service::Status(value) | Service::Stop(value)) =
                parse(&format!("service {action} {selection}")).unwrap()
            else {
                panic!("expected endpoint")
            };
            assert_eq!(value.config.as_deref(), config);
            assert_eq!(value.socket.as_deref(), socket);
        }
    }
    for command in [
        "service",
        "service start",
        "service install",
        "service status --config x --socket /socket",
    ] {
        assert!(parse(command).is_err(), "{command}");
    }
}

#[test]
fn package_commands_share_one_frontend_without_native_dispatch() {
    for input in [
        "install --prefix /private/prefix --bundle /private/bundle",
        "uninstall --prefix /private/prefix",
        "config init --prefix /private/prefix --config /private/aw.yaml --state-dir /private/state --qoder /bin/qoder",
        "config add-provider --config /private/aw.yaml --name external --definition provider.yaml",
    ] {
        assert!(matches!(
            parse(input).unwrap(),
            Command::Install(_) | Command::Uninstall(_) | Command::Config(_)
        ));
    }
}

#[test]
fn mixed_installation_options_are_rejected_before_dispatch() {
    for input in [
        "install --prefix /private/prefix --config local.yaml",
        "install --agent hermes --bundle /private/bundle",
        "install --native-profile work --prefix /private/prefix",
        "install --config local.yaml --agent hermes --bundle /bundle --prefix /prefix",
        "install --dry-run --config local.yaml --agent hermes",
        "install --json --agent hermes",
    ] {
        assert!(parse(input).is_err(), "{input}");
    }
}

#[test]
fn unsupported_agent_commands_do_not_fall_through_to_installation() {
    for input in ["agent", "agent install", "agent disconnect"] {
        assert!(parse(input).is_err(), "{input}");
    }
}

#[test]
fn option_values_do_not_select_another_command() {
    let Command::Install(package) =
        parse("install --prefix=--config --bundle /private/bundle").unwrap()
    else {
        panic!("expected installation")
    };
    assert_eq!(package.prefix, "--config");
}

#[test]
fn run_forwards_native_arguments_without_reinterpreting_them() {
    let Command::Run(run) =
        parse("run --config aw.yaml --agent qoder -- --prefix --bundle --help --json --quiet --verbose --color always").unwrap()
    else {
        panic!("expected launch")
    };
    assert_eq!(
        run.arguments,
        [
            "--prefix",
            "--bundle",
            "--help",
            "--json",
            "--quiet",
            "--verbose",
            "--color",
            "always"
        ]
    );
    assert!(!run.reloadable);
    let Command::Run(run) = parse("run --config aw.yaml --agent qoder --reloadable").unwrap()
    else {
        panic!("expected reloadable launch")
    };
    assert!(run.reloadable);
    let Command::Run(run) = Cli::try_parse_from([
        "aw",
        "run",
        "--config",
        "aw.yaml",
        "--agent",
        "qoder",
        "--",
        "--help",
        "--prefix",
        "x",
        "",
        "a b",
        "$aw.dispatch",
        "--quiet",
        "--verbose",
        "--color",
        "always",
    ])
    .unwrap()
    .command
    else {
        panic!("expected literal launch")
    };
    assert_eq!(
        run.arguments,
        [
            "--help",
            "--prefix",
            "x",
            "",
            "a b",
            "$aw.dispatch",
            "--quiet",
            "--verbose",
            "--color",
            "always"
        ]
    );
}

#[test]
fn command_template_preserves_literal_arguments_without_execution() {
    let Command::Config(Config::Init(config)) = Cli::try_parse_from([
        "aw",
        "config",
        "init",
        "--prefix",
        "/prefix",
        "--config",
        "/aw.yaml",
        "--state-dir",
        "/state",
        "--qoder",
        "/bin/qoder",
        "--provider",
        "command",
        "--check",
        "/unexecuted",
        "--effect",
        "block",
        "--reason-code",
        "denied",
        "--",
        "--help",
        "",
        "a b",
        "$aw.dispatch",
        "--quiet",
        "--verbose",
        "--color",
        "always",
    ])
    .unwrap()
    .command
    else {
        panic!("expected command template")
    };
    assert_eq!(
        config.arguments,
        [
            "--help",
            "",
            "a b",
            "$aw.dispatch",
            "--quiet",
            "--verbose",
            "--color",
            "always"
        ]
    );
}

#[test]
fn invalid_utf8_returns_an_error_without_panicking() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let result =
        crate::package::arguments::decode_all([OsString::from_vec(vec![0xff])].into_iter());
    assert!(result.unwrap_err().to_string().contains("UTF-8"));
}

#[test]
fn idle_policy_update_keeps_intersecting_selection_and_local_options() {
    for command in ["update --instance one", "update --agent qoder"] {
        let Command::Update(value) = parse(command).unwrap() else {
            panic!("expected update");
        };
        assert_eq!(value.config, "aw.yaml");
        assert!(value.socket.is_none() && value.policy.is_none());
    }
    let Command::Update(value) =
        parse("update --socket /socket --instance one --policy next.yaml").unwrap()
    else {
        panic!("expected socket update");
    };
    assert_eq!(value.socket.as_deref(), Some("/socket"));
    assert_eq!(value.policy.as_deref(), Some("next.yaml"));
    assert!(matches!(
        parse("update --config aw.yaml --agent qoder").unwrap(),
        Command::Update(Update { policy: None, .. })
    ));
    assert!(parse("update --config aw.yaml --pid 123").is_err());
    assert!(parse("update --config aw.yaml --pid 123 --agent worker").is_err());
    assert!(parse("update --socket /socket --agent qoder").is_err());
    assert!(matches!(parse("update --config boot.yaml --agent qoder --instance one --policy next.yaml --expected-revision abc").unwrap(), Command::Update(_)));
    for input in [
        "update --config boot.yaml --policy next.yaml",
        "update --config boot.yaml --socket /socket --agent qoder --policy next.yaml",
        "update --config boot.yaml --agent qoder --policy next.yaml --json",
        "request --socket /socket --timeout-ms 0",
        "request --socket /socket --timeout-ms 60001",
    ] {
        assert!(parse(input).is_err(), "{input}");
    }
}

#[test]
fn attachment_options_do_not_leak_into_other_commands() {
    for input in [
        "run --config aw.yaml --agent target --pid 1",
        "run --config aw.yaml --agent target --remove",
        "install --prefix /prefix --bundle /bundle --pid 1",
        "install --prefix /prefix --bundle /bundle --remove",
        "install --config aw.yaml --agent target --pid 1 --json",
    ] {
        assert!(parse(input).is_err(), "{input}");
    }
}
