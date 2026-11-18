//! Select native Hook wiring or cooperative SDK control before runtime dispatch.

use super::dispatch::options;
use crate::cli::Arguments;
use clap::Args;

#[derive(Args, Debug, PartialEq)]
struct Endpoint {
    /// Shared configuration; defaults to aw.yaml without --socket
    #[arg(
        long,
        default_value = "aw.yaml",
        value_name = "FILE",
        conflicts_with = "socket"
    )]
    config: String,
    /// Existing SDK service socket; only for cooperative SDK selection
    #[arg(
        long,
        value_name = "ABS_PATH",
        requires = "sdk_selection",
        conflicts_with = "agent"
    )]
    socket: Option<String>,
    /// SDK acknowledgement deadline in milliseconds; default: 5000 (SDK only)
    #[arg(long, requires = "sdk_selection", conflicts_with = "agent", value_parser = clap::value_parser!(u64).range(100..=30000))]
    timeout_ms: Option<u64>,
}

#[derive(Args, Debug, PartialEq)]
#[command(group(clap::ArgGroup::new("selection").required(true).args(["agent", "sdk_instance", "sdk_pid"])))]
/// Mutually exclusive native and cooperative connection options.
pub(super) struct Connect {
    #[command(flatten)]
    endpoint: Endpoint,
    /// Target declared in the configuration; connect persistent native Hooks
    #[arg(long, value_name = "TARGET")]
    agent: Option<String>,
    /// Attach Hooks to this idle native process; requires --agent
    #[arg(long, requires = "agent", conflicts_with_all = ["native_profile", "sdk_selection"], value_name = "PID", value_parser = clap::value_parser!(u32).range(1..))]
    pid: Option<u32>,
    /// Native framework profile for persistent wiring only
    #[arg(
        long,
        requires = "agent",
        conflicts_with = "sdk_selection",
        value_name = "NAME"
    )]
    native_profile: Option<String>,
    /// Cooperative application identity published by aw instance list
    #[arg(long, value_name = "ID", group = "sdk_selection")]
    sdk_instance: Option<String>,
    /// Find a cooperative SDK application by its live process identity
    #[arg(long, value_name = "PID", group = "sdk_selection", value_parser = clap::value_parser!(u32).range(1..))]
    sdk_pid: Option<u32>,
}

#[derive(Args, Debug, PartialEq)]
#[command(group(clap::ArgGroup::new("selection").required(true).args(["agent", "sdk_instance"])))]
/// Idle native process or exact cooperative application disconnection.
pub(super) struct Disconnect {
    #[command(flatten)]
    endpoint: Endpoint,
    /// Native target from the original connection; requires --pid
    #[arg(long, requires = "pid", value_name = "TARGET")]
    agent: Option<String>,
    /// Release owned Hooks while idle; leave the native process running
    #[arg(long, requires = "agent", conflicts_with = "sdk_selection", value_name = "PID", value_parser = clap::value_parser!(u32).range(1..))]
    pid: Option<u32>,
    /// Release the idle AW connection of this cooperative SDK application
    #[arg(long, value_name = "ID", group = "sdk_selection")]
    sdk_instance: Option<String>,
}

fn sdk(
    endpoint: Endpoint,
    action: &str,
    instance: Option<String>,
    pid: Option<u32>,
) -> (&'static str, Arguments) {
    (
        "sdk-control",
        options([
            ("action", Some(action.into())),
            (
                "config",
                endpoint.socket.is_none().then_some(endpoint.config),
            ),
            ("socket", endpoint.socket),
            ("application", instance),
            ("sdk-pid", pid.map(|pid| pid.to_string())),
            (
                "timeout-ms",
                endpoint.timeout_ms.map(|value| value.to_string()),
            ),
        ]),
    )
}

/// Map admitted connect options to the existing runtime protocol.
pub(super) fn connect(value: Connect) -> (&'static str, Arguments) {
    if value.sdk_instance.is_some() || value.sdk_pid.is_some() {
        return sdk(value.endpoint, "attach", value.sdk_instance, value.sdk_pid);
    }
    (
        "install",
        options([
            ("config", Some(value.endpoint.config)),
            ("agent", value.agent),
            ("pid", value.pid.map(|pid| pid.to_string())),
            ("native-profile", value.native_profile),
        ]),
    )
}

/// Preserve native removal and cooperative detach runtime semantics.
pub(super) fn disconnect(value: Disconnect) -> (&'static str, Arguments) {
    if value.sdk_instance.is_some() {
        return sdk(value.endpoint, "detach", value.sdk_instance, None);
    }
    (
        "install",
        options([
            ("config", Some(value.endpoint.config)),
            ("agent", value.agent),
            ("pid", value.pid.map(|pid| pid.to_string())),
            ("remove", Some("true".into())),
        ]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Cli, Command};
    use clap::Parser;

    fn route(input: &str) -> Result<(&'static str, Arguments), clap::Error> {
        let command =
            Cli::try_parse_from(std::iter::once("aw").chain(input.split_whitespace()))?.command;
        Ok(match command {
            Command::Connect(value) => connect(value),
            Command::Disconnect(value) => disconnect(value),
            _ => panic!("expected connection command"),
        })
    }

    #[test]
    fn native_connections_keep_runtime_arguments() {
        for (input, expected) in [
            (
                "connect --agent target",
                vec![("--config", "aw.yaml"), ("--agent", "target")],
            ),
            (
                "connect --agent target --config=--prefix --native-profile work",
                vec![
                    ("--config", "--prefix"),
                    ("--agent", "target"),
                    ("--native-profile", "work"),
                ],
            ),
            (
                "connect --agent target --pid 123",
                vec![
                    ("--config", "aw.yaml"),
                    ("--agent", "target"),
                    ("--pid", "123"),
                ],
            ),
            (
                "disconnect --config local.yaml --agent target --pid 123",
                vec![
                    ("--config", "local.yaml"),
                    ("--agent", "target"),
                    ("--pid", "123"),
                    ("--remove", "true"),
                ],
            ),
        ] {
            let (command, args) = route(input).unwrap();
            assert_eq!(command, "install");
            assert_eq!(
                args.flags,
                expected
                    .into_iter()
                    .map(|(key, value)| (key.into(), value.into()))
                    .collect()
            );
            assert!(args.native.is_empty());
        }
    }

    #[test]
    fn sdk_connections_keep_runtime_arguments() {
        for (input, action, selection, value, config, timeout) in [
            (
                "connect --sdk-pid 123",
                "attach",
                "--sdk-pid",
                "123",
                true,
                None,
            ),
            (
                "connect --sdk-instance one --socket /socket --timeout-ms 100",
                "attach",
                "--application",
                "one",
                false,
                Some("100"),
            ),
            (
                "disconnect --sdk-instance one --socket /socket",
                "detach",
                "--application",
                "one",
                false,
                None,
            ),
            (
                "disconnect --sdk-instance one --config aw.yaml --timeout-ms 30000",
                "detach",
                "--application",
                "one",
                true,
                Some("30000"),
            ),
        ] {
            let (command, args) = route(input).unwrap();
            assert_eq!(command, "sdk-control");
            let mut expected = vec![("--action", action), (selection, value)];
            if let Some(timeout) = timeout {
                expected.push(("--timeout-ms", timeout));
            }
            expected.push(if config {
                ("--config", "aw.yaml")
            } else {
                ("--socket", "/socket")
            });
            assert_eq!(
                args.flags,
                expected
                    .into_iter()
                    .map(|(key, value)| (key.into(), value.into()))
                    .collect()
            );
            assert!(args.native.is_empty());
        }
    }

    #[test]
    fn connection_modes_and_command_local_options_are_exclusive() {
        for input in [
            "connect",
            "disconnect",
            "connect --pid 1",
            "disconnect --agent target",
            "disconnect --pid 1",
            "disconnect --sdk-pid 1",
            "connect --agent target --remove",
            "connect --agent target --pid 1 --native-profile work",
            "connect --agent target --pid 0",
            "disconnect --agent target --pid 0",
            "connect --sdk-pid 0",
            "connect --sdk-instance one --sdk-pid 1",
            "connect --agent target --sdk-instance one",
            "connect --agent target --sdk-pid 1",
            "connect --sdk-instance one --pid 1",
            "connect --sdk-pid 1 --pid 2",
            "connect --sdk-pid 1 --native-profile work",
            "connect --sdk-instance one --native-profile work",
            "disconnect --sdk-instance one --agent target --pid 1",
            "disconnect --sdk-instance one --pid 1",
            "disconnect --sdk-instance one --remove",
            "connect --sdk-instance one --remove",
            "connect --sdk-pid 1 --remove",
            "disconnect --agent target --pid 1 --native-profile work",
            "connect --agent target --socket /socket",
            "connect --agent target --timeout-ms 5000",
            "disconnect --agent target --pid 1 --socket /socket",
            "disconnect --agent target --pid 1 --timeout-ms 5000",
            "connect --sdk-pid 1 --config aw.yaml --socket /socket",
            "disconnect --sdk-instance one --config aw.yaml --socket /socket",
            "connect --sdk-pid 1 --timeout-ms 99",
            "disconnect --sdk-instance one --timeout-ms 30001",
        ] {
            assert!(route(input).is_err(), "{input}");
        }
    }
}
