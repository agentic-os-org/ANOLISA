//! Native startup assets, SDK inventory and the application-owned JSON protocol.

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
    /// Install one-time native startup wiring in an explicit private directory
    Setup(Setup),
    /// Prepare bridge assets for the calling application's native factory
    Prepare(Prepare),
    /// Release the calling application's registration and staged bridge assets
    Release(Release),
    /// List live applications that published cooperative SDK support
    List(SdkList),
}

#[derive(Args, Debug, PartialEq)]
struct Prepare {
    /// Shared configuration
    #[arg(long, default_value = "aw.yaml", value_name = "FILE")]
    config: String,
    /// Target declared in the configuration
    #[arg(long, value_name = "TARGET")]
    agent: String,
    /// Existing native settings file
    #[arg(long)]
    native_settings: Option<String>,
    /// Existing native profile
    #[arg(long)]
    native_profile: Option<String>,
    /// Existing native state directory
    #[arg(long)]
    native_state_dir: Option<String>,
}

#[derive(Args, Debug, PartialEq)]
struct Setup {
    #[command(flatten)]
    prepare: Prepare,
    /// Private startup directory or dedicated venv site-packages
    #[arg(long, value_name = "ABS_DIR")]
    directory: String,
    /// Remove an inactive Qoder startup installation, preserving user Hooks
    #[arg(long)]
    remove: bool,
}

#[derive(Args, Debug, PartialEq)]
struct Release {
    /// Private binding returned to the application by prepare
    #[arg(long, value_name = "FILE")]
    binding: String,
}

#[derive(Args, Debug, PartialEq)]
struct SdkList {
    /// Shared configuration, used only to find an existing service
    #[arg(long, default_value = "aw.yaml", conflicts_with = "socket")]
    config: String,
    /// Existing private service socket
    #[arg(long)]
    socket: Option<String>,
}

pub(super) fn run(value: Instance) -> Result<Exit> {
    match value.action {
        Some(Action::Setup(value)) => crate::runtime(
            "native-setup",
            &options([
                ("config", Some(value.prepare.config)),
                ("agent", Some(value.prepare.agent)),
                ("directory", Some(value.directory)),
                ("remove", value.remove.then(|| "true".into())),
                ("native-settings", value.prepare.native_settings),
                ("native-profile", value.prepare.native_profile),
                ("native-state-dir", value.prepare.native_state_dir),
            ]),
        ),
        Some(Action::Prepare(value)) => crate::runtime(
            "native-prepare",
            &options([
                ("config", Some(value.config)),
                ("agent", Some(value.agent)),
                ("native-settings", value.native_settings),
                ("native-profile", value.native_profile),
                ("native-state-dir", value.native_state_dir),
            ]),
        ),
        Some(Action::Release(value)) => crate::runtime(
            "native-release",
            &options([("binding", Some(value.binding))]),
        ),
        Some(Action::List(value)) => crate::runtime(
            "sdk-control",
            &options([
                ("action", Some("list".into())),
                ("config", value.socket.is_none().then_some(value.config)),
                ("socket", value.socket),
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
        assert!(parse(&["aw", "instance", "update", "--instance", "one"]).is_err());
        let value = parse(&[
            "aw",
            "instance",
            "setup",
            "--agent",
            "target",
            "--directory",
            "/private",
        ])
        .unwrap();
        let Some(Action::Setup(value)) = value.action else {
            panic!("expected setup")
        };
        assert_eq!(value.prepare.config, "aw.yaml");
        assert_eq!(value.directory, "/private");
    }

    #[test]
    fn sdk_inventory_retains_explicit_endpoint_selection() {
        parse(&["aw", "instance", "list", "--socket", "/socket"]).unwrap();
        parse(&["aw", "instance", "list"]).unwrap();
        assert!(
            parse(&["aw", "instance", "list", "--socket", "/socket", "--config", "aw.yaml"])
                .is_err()
        );
    }

    #[test]
    fn invalid_or_mixed_management_modes_are_rejected() {
        for tail in [
            vec![],
            vec!["update"],
            vec!["detach"],
            vec!["attach", "--sdk-instance", "one", "--pid", "123"],
            vec!["attach", "--sdk-instance", "one", "--sdk-pid", "123"],
            vec!["attach", "--agent", "target", "--sdk-pid", "123"],
            vec!["attach", "--sdk-pid", "123", "--remove"],
            vec!["attach", "--sdk-instance", "one", "--remove"],
            vec!["attach", "--sdk-instance", "one", "--timeout-ms", "99"],
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
