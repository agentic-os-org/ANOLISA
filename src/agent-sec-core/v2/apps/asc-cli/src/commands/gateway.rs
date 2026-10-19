//! Secret gateway registration, administration and scoped credential queries.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use asc_daemon_protocol::method::{
    GATEWAY_CREDENTIALS, GATEWAY_DISABLE, GATEWAY_ENABLE, GATEWAY_KEY_ADD, GATEWAY_KEY_LIST,
    GATEWAY_KEY_REMOVE, GATEWAY_REGISTER, GATEWAY_STATUS, GATEWAY_UNREGISTER,
};
use asc_daemon_protocol::{
    DaemonRequest, GatewayAgentParams, GatewayKeyAddParams, GatewayKeyRemoveParams,
    GatewayRegisterParams,
};
use clap::{Args, Subcommand};

use super::common::encode;
use crate::InputError;
use crate::output::render_policy;

#[derive(Debug, Subcommand)]
pub(crate) enum GatewayCommand {
    /// Start the secret gateway with the configured keys (root only).
    Enable,
    /// Stop the secret gateway and remove its runtime files (root only).
    Disable,
    /// Show gateway state and domains visible to your UID, without key values.
    Status,
    /// Manage protected keys (root only).
    #[command(subcommand)]
    Key(KeyCommand),
    /// Register an existing Agent process and its descendants (root only).
    Register(RegisterArgs),
    /// Withdraw an Agent domain and all its keys without stopping its processes (root only).
    Unregister(AgentArgs),
    /// Read your domain's fake keys and CA environment; does not launch or move a process.
    Credentials(AgentArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum KeyCommand {
    /// Protect a preset key for a registered Agent domain.
    Add(KeyAdd),
    /// Withdraw a preset key from an Agent domain.
    Remove(KeyRemove),
    /// List protected keys with abbreviated fakes.
    List,
}

#[derive(Debug, Args)]
pub(crate) struct KeyAdd {
    /// Built-in preset; currently only `github`.
    preset: String,
    /// Registered Agent domain name (lowercase DNS label).
    #[arg(long)]
    agent: String,
    /// Root-owned file holding the real key, unreadable by group and others.
    #[arg(long)]
    secret_file: PathBuf,
}

#[derive(Debug, Args)]
pub(crate) struct KeyRemove {
    /// Built-in preset.
    preset: String,
    /// Agent domain name.
    #[arg(long)]
    agent: String,
}

#[derive(Debug, Args)]
pub(crate) struct RegisterArgs {
    /// Agent domain name (lowercase DNS label).
    #[arg(long)]
    agent: String,
    /// Non-root UID owning the Agent process; verified by the daemon.
    #[arg(long)]
    uid: u32,
    /// PID of the already running Agent; its existing descendants are also registered.
    #[arg(long)]
    pid: u32,
}

#[derive(Debug, Args)]
pub(crate) struct AgentArgs {
    /// Agent domain name.
    #[arg(long)]
    agent: String,
}

impl GatewayCommand {
    pub(crate) fn run(
        &self,
        socket: &Path,
        timeout: Duration,
        stdout: &mut impl Write,
        stderr: &mut impl Write,
    ) -> io::Result<u8> {
        let request = self.request().map_err(io::Error::other)?;
        let response =
            asc_daemon_client::call(socket, &request, timeout).map_err(io::Error::other)?;
        render_policy(&response, stdout, stderr)
    }

    fn request(&self) -> Result<DaemonRequest, InputError> {
        let empty = serde_json::json!({});
        match self {
            Self::Enable => encode(GATEWAY_ENABLE, &empty),
            Self::Disable => encode(GATEWAY_DISABLE, &empty),
            Self::Status => encode(GATEWAY_STATUS, &empty),
            Self::Register(args) => encode(
                GATEWAY_REGISTER,
                &GatewayRegisterParams {
                    agent: args.agent.clone(),
                    uid: args.uid,
                    pid: args.pid,
                },
            ),
            Self::Unregister(args) => encode(
                GATEWAY_UNREGISTER,
                &GatewayAgentParams {
                    agent: args.agent.clone(),
                },
            ),
            Self::Credentials(args) => encode(
                GATEWAY_CREDENTIALS,
                &GatewayAgentParams {
                    agent: args.agent.clone(),
                },
            ),
            Self::Key(KeyCommand::List) => encode(GATEWAY_KEY_LIST, &empty),
            Self::Key(KeyCommand::Add(args)) => encode(
                GATEWAY_KEY_ADD,
                &GatewayKeyAddParams {
                    agent: args.agent.clone(),
                    preset: args.preset.clone(),
                    secret_file: std::path::absolute(&args.secret_file)?
                        .to_str()
                        .ok_or_else(|| InputError::Gateway("--secret-file must be UTF-8".into()))?
                        .to_owned(),
                },
            ),
            Self::Key(KeyCommand::Remove(args)) => encode(
                GATEWAY_KEY_REMOVE,
                &GatewayKeyRemoveParams {
                    agent: args.agent.clone(),
                    preset: args.preset.clone(),
                },
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Arguments {
        #[command(subcommand)]
        command: GatewayCommand,
    }

    #[test]
    fn registration_encodes_the_target_not_the_cli_process() {
        let args = Arguments::try_parse_from([
            "gateway", "register", "--agent", "dev", "--uid", "1001", "--pid", "4242",
        ])
        .unwrap();
        let request = args.command.request().unwrap();
        assert_eq!(request.method.as_str(), GATEWAY_REGISTER);
        assert_eq!(
            request.params,
            serde_json::json!({"agent": "dev", "uid": 1001, "pid": 4242})
        );
    }

    #[test]
    fn credential_queries_do_not_accept_a_caller_selected_uid() {
        let args = Arguments::try_parse_from(["gateway", "credentials", "--agent", "dev"]).unwrap();
        let request = args.command.request().unwrap();
        assert_eq!(request.method.as_str(), GATEWAY_CREDENTIALS);
        assert_eq!(request.params, serde_json::json!({"agent": "dev"}));
        assert!(
            Arguments::try_parse_from([
                "gateway",
                "credentials",
                "--agent",
                "dev",
                "--uid",
                "1001",
            ])
            .is_err()
        );
    }
}
