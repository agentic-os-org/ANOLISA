use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::path::PathBuf;

use asc_foundation_types::{DAEMON_SOCKET_ENV, daemon_socket_path_from_env};

use crate::BootstrapConfig;

const HELP: &str = "Usage: agent-sec-daemon [serve] [--socket <ABSOLUTE_PATH>] [--policy-admin-uid <UID>]...\n\
\n\
Runs the AgentSecCore V2 UDS service with PAP administration methods.\n\
Without --socket, uses nonempty $AGENT_SEC_DAEMON_SOCKET or /run/agent-sec-core/daemon.sock.\n\
Root is always authorized. --policy-admin-uid adds an administrator at startup.\n\
Repeat this option for multiple UIDs; omitted means root only.\n\
PAP state is process-local until durable Repository integration lands.\n";

/// Parsed command-line configuration for the daemon process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cli {
    /// Bootstrap configuration selected by the explicit process invocation.
    pub bootstrap: BootstrapConfig,
    /// Additional administrator UIDs selected by the daemon deployment operator.
    pub policy_admin_uids: BTreeSet<u32>,
}

/// Successful command-line parse outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseOutcome {
    /// Run the foreground daemon service.
    Serve(Cli),
    /// Print help without starting the service.
    Help(&'static str),
}

impl Cli {
    /// Parses an argv sequence including the binary name.
    ///
    /// Accepts both direct and explicit `serve` forms. When `--socket` is
    /// omitted, the deployment-provided `AGENT_SEC_DAEMON_SOCKET` selects the
    /// endpoint; explicit paths remain available for tests and tools.
    ///
    /// # Errors
    /// Returns a stable parse error for a missing value, unknown option, repeated
    /// socket, invalid administrator UID, non-Unicode option, or invalid runtime path.
    pub fn parse_from<I, T>(arguments: I) -> Result<ParseOutcome, CliError>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString>,
    {
        let socket_env = std::env::var_os(DAEMON_SOCKET_ENV);
        Self::parse_from_with_socket_env(arguments, socket_env.as_deref())
    }

    fn parse_from_with_socket_env<I, T>(
        arguments: I,
        socket_env: Option<&OsStr>,
    ) -> Result<ParseOutcome, CliError>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString>,
    {
        let mut arguments = arguments.into_iter().map(Into::into);
        let _program = arguments.next();
        let mut socket_path = None;
        let mut command_seen = false;
        let mut policy_admin_uids = BTreeSet::new();

        while let Some(argument) = arguments.next() {
            if argument == OsStr::new("--help") || argument == OsStr::new("-h") {
                return Ok(ParseOutcome::Help(HELP));
            }
            if argument == OsStr::new("serve") && !command_seen && socket_path.is_none() {
                command_seen = true;
                continue;
            }
            if argument == OsStr::new("--socket") {
                if socket_path.is_some() {
                    return Err(CliError::RepeatedSocket);
                }
                let value = arguments.next().ok_or(CliError::MissingSocketValue)?;
                if value.is_empty() {
                    return Err(CliError::MissingSocketValue);
                }
                socket_path = Some(PathBuf::from(value));
                continue;
            }
            let inline_uid = argument
                .to_str()
                .and_then(|value| value.strip_prefix("--policy-admin-uid="));
            if argument == OsStr::new("--policy-admin-uid") || inline_uid.is_some() {
                let value = if let Some(value) = inline_uid {
                    OsString::from(value)
                } else {
                    arguments.next().ok_or(CliError::MissingAdminUid)?
                };
                let value = value.to_str().ok_or(CliError::InvalidAdminUid)?;
                if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(CliError::InvalidAdminUid);
                }
                policy_admin_uids.insert(
                    value
                        .parse::<u32>()
                        .map_err(|_| CliError::InvalidAdminUid)?,
                );
                continue;
            }

            let mut rendered = String::new();
            write!(&mut rendered, "{}", argument.to_string_lossy())
                .expect("writing into a String cannot fail");
            return Err(CliError::UnknownArgument(rendered));
        }

        let socket_path = if let Some(path) = socket_path {
            path
        } else {
            daemon_socket_path_from_env(
                socket_env
                    .filter(|path| !path.is_empty())
                    .or(Some(OsStr::new("/run/agent-sec-core/daemon.sock"))),
            )
            .map_err(|_| CliError::RelativeSocket)?
        };
        if !socket_path.is_absolute() {
            return Err(CliError::RelativeSocket);
        }
        let mut bootstrap = BootstrapConfig::new(socket_path);
        // The host service accepts local users; embedders retain a private default.
        bootstrap.socket_mode = 0o666;
        Ok(ParseOutcome::Serve(Self {
            bootstrap,
            policy_admin_uids,
        }))
    }
}

/// Invalid daemon command-line input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CliError {
    /// A startup administrator option was not followed by a UID.
    #[error("--policy-admin-uid requires a UID")]
    MissingAdminUid,
    /// Kernel UIDs are unsigned 32-bit decimal values.
    #[error("--policy-admin-uid must be a decimal integer between 0 and 4294967295")]
    InvalidAdminUid,
    /// `--socket` was not followed by a value.
    #[error("--socket requires a value")]
    MissingSocketValue,
    /// Supplying multiple socket paths is ambiguous.
    #[error("--socket may be specified only once")]
    RepeatedSocket,
    /// The service framework rejects relative daemon endpoints.
    #[error("--socket must be an absolute path")]
    RelativeSocket,
    /// An unsupported process option was supplied.
    #[error("unknown argument: {0}")]
    UnknownArgument(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_subcommand_and_serve_select_the_same_foreground_process() {
        let direct = Cli::parse_from(["agent-sec-daemon", "--socket", "/run/asc/daemon.sock"]);
        let explicit = Cli::parse_from([
            "agent-sec-daemon",
            "serve",
            "--socket",
            "/run/asc/daemon.sock",
        ]);

        assert_eq!(direct, explicit);
        assert!(matches!(direct, Ok(ParseOutcome::Serve(_))));
    }

    #[test]
    fn socket_uses_the_deployment_environment_or_an_explicit_absolute_path() {
        let ParseOutcome::Serve(default) = Cli::parse_from_with_socket_env(
            ["agent-sec-daemon", "serve"],
            Some(OsStr::new("/run/agent-sec-core/daemon.sock")),
        )
        .unwrap() else {
            panic!("expected daemon invocation");
        };
        assert_eq!(default.bootstrap.socket_mode, 0o666);
        assert_eq!(BootstrapConfig::new("/run/private.sock").socket_mode, 0o600);
        assert_eq!(
            default.bootstrap.socket_path,
            PathBuf::from("/run/agent-sec-core/daemon.sock")
        );
        assert_eq!(
            Cli::parse_from_with_socket_env(["agent-sec-daemon"], None).unwrap(),
            Cli::parse_from_with_socket_env(
                ["agent-sec-daemon"],
                Some(OsStr::new("/run/agent-sec-core/daemon.sock"))
            )
            .unwrap()
        );
        assert_eq!(
            Cli::parse_from_with_socket_env(["agent-sec-daemon"], Some(OsStr::new("relative"))),
            Err(CliError::RelativeSocket)
        );
        let ParseOutcome::Serve(explicit) = Cli::parse_from_with_socket_env(
            ["agent-sec-daemon", "--socket", "/run/explicit.sock"],
            Some(OsStr::new("/run/agent-sec-core/daemon.sock")),
        )
        .unwrap() else {
            panic!("expected daemon invocation");
        };
        assert_eq!(
            explicit.bootstrap.socket_path,
            PathBuf::from("/run/explicit.sock")
        );
        assert_eq!(
            Cli::parse_from_with_socket_env(
                [
                    "agent-sec-daemon",
                    "--socket",
                    "/run/one.sock",
                    "--socket",
                    "/run/two.sock",
                ],
                None,
            ),
            Err(CliError::RepeatedSocket)
        );
    }

    #[test]
    fn administrator_uids_are_explicit_repeatable_and_bounded() {
        let ParseOutcome::Serve(default) =
            Cli::parse_from(["agent-sec-daemon", "--socket", "/run/asc.sock"]).unwrap()
        else {
            panic!("expected daemon invocation");
        };
        assert!(default.policy_admin_uids.is_empty());
        let ParseOutcome::Serve(configured) = Cli::parse_from([
            "agent-sec-daemon",
            "serve",
            "--socket",
            "/run/asc.sock",
            "--policy-admin-uid",
            "1000",
            "--policy-admin-uid=2000",
            "--policy-admin-uid",
            "1000",
            "--policy-admin-uid=0",
        ])
        .unwrap() else {
            panic!("expected daemon invocation");
        };
        assert_eq!(
            configured.policy_admin_uids,
            BTreeSet::from([0, 1000, 2000])
        );
        for value in ["", "-1", "+1", "4294967296", "root", "1,2", "--help"] {
            assert_eq!(
                Cli::parse_from([
                    "agent-sec-daemon",
                    "--socket",
                    "/run/asc.sock",
                    "--policy-admin-uid",
                    value,
                ]),
                Err(CliError::InvalidAdminUid)
            );
        }
        assert_eq!(
            Cli::parse_from([
                "agent-sec-daemon",
                "--socket",
                "/run/asc.sock",
                "--policy-admin-uid",
            ]),
            Err(CliError::MissingAdminUid)
        );
    }
}
