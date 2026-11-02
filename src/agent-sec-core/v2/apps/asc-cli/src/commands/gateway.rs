//! Root credential administration and owner-scoped startup environment queries.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use asc_daemon_protocol::method::{
    GATEWAY_CREDENTIALS, GATEWAY_DISABLE, GATEWAY_ENABLE, GATEWAY_KEY_ADD, GATEWAY_KEY_LIST,
    GATEWAY_KEY_REMOVE, GATEWAY_STATUS,
};
use asc_daemon_protocol::{
    DaemonRequest, GatewayKeyAddParams, GatewayKeyRemoveParams, GatewayScopeParams,
};
use clap::{Args, Subcommand};

use super::common::encode;
use crate::InputError;
use crate::output::render_policy;

#[derive(Debug, Subcommand)]
pub(crate) enum GatewayCommand {
    /// Enable the credential gateway (root only).
    Enable,
    /// Withdraw routing and stop the credential gateway (root only).
    Disable,
    /// Show execution state visible to your UID, without credential values.
    Status,
    /// Import, list or globally retire credentials (root only).
    #[command(subcommand)]
    Key(KeyCommand),
    /// Query your Scope's fake credentials and CA environment.
    Credentials(ScopeArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum KeyCommand {
    /// Import a credential without replacing an existing value or granting Scope access.
    Add(KeyAdd),
    /// Globally retire a credential and revoke all referencing grants.
    Remove(KeyRemove),
    /// List currently imported credential identifiers.
    List,
}

#[derive(Debug, Args)]
pub(crate) struct KeyAdd {
    /// Credential identifier; currently `github`.
    credential: String,
    /// Root-owned file holding the real value, unreadable by group and others.
    #[arg(long)]
    secret_file: PathBuf,
}

#[derive(Debug, Args)]
pub(crate) struct KeyRemove {
    /// Credential identifier.
    credential: String,
}

#[derive(Debug, Args)]
pub(crate) struct ScopeArgs {
    /// Canonical Scope identity returned by `scope create`.
    #[arg(long)]
    scope: String,
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
            Self::Credentials(args) => encode(
                GATEWAY_CREDENTIALS,
                &GatewayScopeParams {
                    scope_id: args.scope.clone(),
                },
            ),
            Self::Key(KeyCommand::List) => encode(GATEWAY_KEY_LIST, &empty),
            Self::Key(KeyCommand::Add(args)) => encode(
                GATEWAY_KEY_ADD,
                &GatewayKeyAddParams {
                    credential: args.credential.clone(),
                    secret_file: std::path::absolute(&args.secret_file)?
                        .to_str()
                        .ok_or_else(|| InputError::Gateway("--secret-file must be UTF-8".into()))?
                        .to_owned(),
                },
            ),
            Self::Key(KeyCommand::Remove(args)) => encode(
                GATEWAY_KEY_REMOVE,
                &GatewayKeyRemoveParams {
                    credential: args.credential.clone(),
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
    fn credential_import_is_independent_of_process_or_scope_admission() {
        let args = Arguments::try_parse_from([
            "gateway",
            "key",
            "add",
            "github",
            "--secret-file",
            "/root/token",
        ])
        .unwrap();
        let request = args.command.request().unwrap();
        assert_eq!(request.method.as_str(), GATEWAY_KEY_ADD);
        assert_eq!(
            request.params,
            serde_json::json!({"credential":"github","secretFile":"/root/token"})
        );
    }

    #[test]
    fn environment_queries_select_scope_but_cannot_select_caller_identity() {
        let args =
            Arguments::try_parse_from(["gateway", "credentials", "--scope", "scope-1"]).unwrap();
        let request = args.command.request().unwrap();
        assert_eq!(request.method.as_str(), GATEWAY_CREDENTIALS);
        assert_eq!(request.params, serde_json::json!({"scopeId":"scope-1"}));
        assert!(
            Arguments::try_parse_from([
                "gateway",
                "credentials",
                "--scope",
                "scope-1",
                "--uid",
                "1001",
            ])
            .is_err()
        );
    }
}
