//! Instance management reuses native attachment and atomic policy activation.

use super::dispatch::options;
use crate::cli::{Exit, Result};
use clap::{Args, Subcommand};

#[derive(Args, Debug, PartialEq)]
#[command(subcommand_negates_reqs = true, args_conflicts_with_subcommands = true)]
pub(super) struct Instance {
    #[command(subcommand)]
    action: Option<Action>,
    /// Unix socket for the existing application-owned stdin JSON protocol
    #[arg(long, required = true)]
    socket: Option<String>,
    /// Discover protocol and generation without reading stdin
    #[arg(long)]
    connect: bool,
    /// Total input, connection and request budget for stdin JSON
    #[arg(long, default_value_t = 30000, value_parser = clap::value_parser!(u64).range(1..=60000))]
    timeout_ms: u64,
}

#[derive(Subcommand, Debug, PartialEq)]
enum Action {
    /// Install supported native Hooks in an explicitly selected process
    Attach(Attach),
    /// Apply policy to an already connected idle instance
    Update(Update),
}

#[derive(Args, Debug, PartialEq)]
struct Attach {
    /// Shared configuration; defaults to aw.yaml in the current directory
    #[arg(long, default_value = "aw.yaml", value_name = "FILE")]
    config: String,
    /// Target declared in the configuration
    #[arg(long, value_name = "TARGET")]
    agent: String,
    /// Explicit native process; identity and native adoption are checked
    #[arg(long, value_name = "PID", value_parser = clap::value_parser!(u32).range(1..))]
    pid: u32,
    /// Remove owned Hooks while idle and leave the process running
    #[arg(long)]
    remove: bool,
}

#[derive(Args, Debug, PartialEq)]
struct Update {
    /// Shared configuration; defaults to aw.yaml in the current directory
    #[arg(long, default_value = "aw.yaml", value_name = "FILE")]
    config: String,
    /// Target declared in the configuration
    #[arg(long, required_unless_present = "instance", value_name = "TARGET")]
    agent: Option<String>,
    /// Select one instance, optionally intersecting the configured target
    #[arg(long, required_unless_present = "agent", value_name = "ID")]
    instance: Option<String>,
    /// Separate candidate; defaults to the shared configuration
    #[arg(long, value_name = "FILE")]
    policy: Option<String>,
    /// Expected active policy revision; conflicts are not retried
    #[arg(long, value_name = "SHA256")]
    expected_revision: Option<String>,
}

pub(super) fn run(value: Instance) -> Result<Exit> {
    match value.action {
        Some(Action::Attach(value)) => crate::runtime(
            "install",
            &options([
                ("config", Some(value.config)),
                ("agent", Some(value.agent)),
                ("pid", Some(value.pid.to_string())),
                ("remove", value.remove.then(|| "true".into())),
            ]),
        ),
        Some(Action::Update(value)) => crate::runtime(
            "update",
            &options([
                ("config", Some(value.config)),
                ("agent", value.agent),
                ("instance", value.instance),
                ("policy", value.policy),
                ("expected-revision", value.expected_revision),
            ]),
        ),
        None => crate::cli::instance::run(
            value.socket.as_deref().ok_or("missing --socket")?,
            value.connect,
            value.timeout_ms,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse(arguments: &[&str]) -> std::result::Result<Instance, clap::Error> {
        let super::super::Command::Instance(value) =
            super::super::Cli::try_parse_from(arguments)?.command
        else {
            panic!("expected instance command");
        };
        Ok(value)
    }

    #[test]
    fn management_defaults_do_not_change_the_registration_protocol() {
        let legacy = parse(&["aw", "instance", "--socket", "/socket", "--connect"]).unwrap();
        assert!(legacy.action.is_none() && legacy.connect);
        assert_eq!(legacy.timeout_ms, 30000);
        let value = parse(&[
            "aw", "instance", "attach", "--agent", "target", "--pid", "123",
        ])
        .unwrap();
        let Some(Action::Attach(value)) = value.action else {
            panic!("expected attach")
        };
        assert_eq!(value.config, "aw.yaml");
        assert_eq!(value.pid, 123);
        let value = parse(&["aw", "instance", "update", "--instance", "one"]).unwrap();
        let Some(Action::Update(value)) = value.action else {
            panic!("expected update")
        };
        assert_eq!(value.config, "aw.yaml");
        assert_eq!(value.instance.as_deref(), Some("one"));
    }

    #[test]
    fn invalid_or_mixed_management_modes_are_rejected() {
        for tail in [
            vec![],
            vec!["update"],
            vec!["attach", "--agent", "target", "--pid", "0"],
            vec!["update", "--agent", "target", "--pid", "123"],
            vec!["--socket", "/socket", "update", "--instance", "one"],
            vec!["--connect", "attach", "--agent", "target", "--pid", "123"],
        ] {
            let mut argv = vec!["aw", "instance"];
            argv.extend(tail);
            assert!(parse(&argv).is_err(), "{argv:?}");
        }
    }
}
