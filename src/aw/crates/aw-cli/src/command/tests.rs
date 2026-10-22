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
fn native_install_and_connect_share_exact_runtime_arguments() {
    let Command::Install(old) =
        parse("install --config local.yaml --agent hermes --native-profile work").unwrap()
    else {
        panic!("expected legacy installation")
    };
    let Command::Agent(Agent::Connect(new)) =
        parse("agent connect --config local.yaml --agent hermes --native-profile work").unwrap()
    else {
        panic!("expected native connection")
    };
    assert_eq!(old.config, Some(new.config));
    assert_eq!(old.agent, Some(new.agent));
    assert_eq!(old.native_profile, new.native_profile);
    assert!(old.prefix.is_none() && old.bundle.is_none());
}

#[test]
fn configuration_validation_keeps_legacy_scope_comparison() {
    let Command::Validate(old) =
        parse("validate --config candidate.yaml --against original.yaml").unwrap()
    else {
        panic!("expected legacy validation")
    };
    let Command::Config(Config::Validate(new)) =
        parse("config validate --config candidate.yaml --against original.yaml").unwrap()
    else {
        panic!("expected config validation")
    };
    assert_eq!(old, new.validation);
}

#[test]
fn service_groups_preserve_runtime_arguments() {
    let Command::Serve(old) = parse("serve --config aw.yaml --state-dir /private/state").unwrap()
    else {
        panic!("expected legacy serve")
    };
    let Command::Service(Service::Serve(new)) =
        parse("service serve --config aw.yaml --state-dir /private/state").unwrap()
    else {
        panic!("expected serve")
    };
    assert_eq!(old, new);
    for (old, new) in [
        (
            parse("status --config aw.yaml"),
            parse("service status --config aw.yaml"),
        ),
        (
            parse("stop --config aw.yaml"),
            parse("service stop --config aw.yaml"),
        ),
    ] {
        let old = match old.unwrap() {
            Command::Status(value) | Command::Stop(value) => value,
            _ => panic!("expected legacy endpoint"),
        };
        let new = match new.unwrap() {
            Command::Service(Service::Status(value) | Service::Stop(value)) => value,
            _ => panic!("expected endpoint"),
        };
        assert_eq!(old, new);
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
        assert!(matches!(parse(input).unwrap(), Command::Install(_) | Command::Uninstall(_) | Command::Config(_)));
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
fn option_values_do_not_select_another_installation_mode() {
    let Command::Install(native) = parse("install --config=--prefix --agent target").unwrap()
    else {
        panic!("expected installation")
    };
    assert_eq!(native.config.as_deref(), Some("--prefix"));
    assert!(native.prefix.is_none());
    let Command::Install(package) =
        parse("install --prefix=--config --bundle /private/bundle").unwrap()
    else {
        panic!("expected installation")
    };
    assert_eq!(package.prefix.as_deref(), Some("--config"));
    assert!(package.config.is_none());
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
