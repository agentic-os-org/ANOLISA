//! Gateway administration and shared public CA queries.

use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use asc_daemon_protocol::method::{
    GATEWAY_CA, GATEWAY_DISABLE, GATEWAY_ENABLE, GATEWAY_KEY_LIST, GATEWAY_KEY_REMOVE,
    GATEWAY_STATUS,
};
use asc_daemon_protocol::{DaemonRequest, GatewayKeyRemoveParams};
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
    /// List or globally retire credential pairs (root only).
    #[command(subcommand)]
    Key(KeyCommand),
    /// Return the shared public CA bundle path while the gateway is ready.
    Ca,
}

#[derive(Debug, Subcommand)]
pub(crate) enum KeyCommand {
    /// Globally retire a pair and revoke all referencing grants.
    Remove(KeyRemove),
    /// List currently imported pair identifiers, without token values.
    List,
}

#[derive(Debug, Args)]
pub(crate) struct KeyRemove {
    /// Internal pair identifier.
    pairid: String,
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
            Self::Ca => encode(GATEWAY_CA, &empty),
            Self::Key(KeyCommand::List) => encode(GATEWAY_KEY_LIST, &empty),
            Self::Key(KeyCommand::Remove(args)) => encode(
                GATEWAY_KEY_REMOVE,
                &GatewayKeyRemoveParams {
                    pairid: args.pairid.clone(),
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
    fn removal_selects_an_internal_pair_identifier() {
        let args = Arguments::try_parse_from(["gateway", "key", "remove", "pair-1"]).unwrap();
        let request = args.command.request().unwrap();
        assert_eq!(request.method.as_str(), GATEWAY_KEY_REMOVE);
        assert_eq!(request.params, serde_json::json!({"pairid":"pair-1"}));
    }

    #[test]
    fn ca_queries_cannot_select_scope_or_caller_identity() {
        let args = Arguments::try_parse_from(["gateway", "ca"]).unwrap();
        let request = args.command.request().unwrap();
        assert_eq!(request.method.as_str(), GATEWAY_CA);
        assert_eq!(request.params, serde_json::json!({}));
        for option in ["--scope", "--uid", "--gid", "--pid"] {
            assert!(Arguments::try_parse_from(["gateway", "ca", option, "1001"]).is_err());
        }
    }
}
