//! Gateway administration and shared public CA queries.

use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use asc_daemon_protocol::method::{
    GATEWAY_CA, GATEWAY_DISABLE, GATEWAY_ENABLE, GATEWAY_KEY_LIST, GATEWAY_KEY_REMOVE,
    GATEWAY_STATUS,
};
use asc_daemon_protocol::{DaemonRequest, DaemonResponse, GatewayKeyRemoveParams};
use clap::{Args, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    /// Return shared public PEM and PKCS#12 trust bundle paths while the gateway is ready.
    Ca(CaArgs),
}

#[derive(Debug, Args)]
pub(crate) struct CaArgs {
    /// Select one format to display; omit to return both formats.
    #[arg(long, value_enum)]
    format: Option<CaFormat>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CaFormat {
    Pem,
    Pkcs12,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CaResult {
    ca_bundle_paths: CaPaths,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CaPaths {
    pem: String,
    pkcs12: String,
}

/// Validates the complete daemon response before any consumer selects one format.
pub(super) fn ca_bundle_paths(result: &Value) -> io::Result<CaPaths> {
    let decoded = CaResult::deserialize(result)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid gateway CA result"))?;
    for path in [
        &decoded.ca_bundle_paths.pem,
        &decoded.ca_bundle_paths.pkcs12,
    ] {
        if !Path::new(path).is_absolute() || path.contains('\0') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "gateway CA paths must be valid absolute paths",
            ));
        }
    }
    Ok(decoded.ca_bundle_paths)
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
        self.render(response, stdout, stderr)
    }

    fn render(
        &self,
        mut response: DaemonResponse,
        stdout: &mut impl Write,
        stderr: &mut impl Write,
    ) -> io::Result<u8> {
        if let Self::Ca(args) = self
            && let DaemonResponse::Success(success) = &mut response
        {
            let paths = ca_bundle_paths(&success.result)?;
            let paths = match args.format {
                None => serde_json::json!(paths),
                Some(CaFormat::Pem) => serde_json::json!({"pem": paths.pem}),
                Some(CaFormat::Pkcs12) => serde_json::json!({"pkcs12": paths.pkcs12}),
            };
            success.result = serde_json::json!({"caBundlePaths": paths});
        }
        render_policy(&response, stdout, stderr)
    }

    fn request(&self) -> Result<DaemonRequest, InputError> {
        let empty = serde_json::json!({});
        match self {
            Self::Enable => encode(GATEWAY_ENABLE, &empty),
            Self::Disable => encode(GATEWAY_DISABLE, &empty),
            Self::Status => encode(GATEWAY_STATUS, &empty),
            Self::Ca(_) => encode(GATEWAY_CA, &empty),
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
        assert!(Arguments::try_parse_from(["gateway", "ca", "--format", "der"]).is_err());
    }

    #[test]
    fn ca_format_selects_output_only_and_preserves_daemon_errors() {
        let result =
            serde_json::json!({"caBundlePaths": {"pem": "/run/ca.pem", "pkcs12": "/run/ca.p12"}});
        for (argv, expected) in [
            (vec!["gateway", "ca"], result.clone()),
            (
                vec!["gateway", "ca", "--format", "pem"],
                serde_json::json!({"caBundlePaths": {"pem": "/run/ca.pem"}}),
            ),
            (
                vec!["gateway", "ca", "--format", "pkcs12"],
                serde_json::json!({"caBundlePaths": {"pkcs12": "/run/ca.p12"}}),
            ),
        ] {
            let command = Arguments::try_parse_from(argv).unwrap().command;
            assert_eq!(command.request().unwrap().params, serde_json::json!({}));
            let id = asc_daemon_protocol::RequestId::new("ca-test").unwrap();
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            assert_eq!(
                command
                    .render(
                        DaemonResponse::success(id.clone(), result.clone()),
                        &mut stdout,
                        &mut stderr
                    )
                    .unwrap(),
                0
            );
            assert_eq!(serde_json::from_slice::<Value>(&stdout).unwrap(), expected);
            assert!(stderr.is_empty());
            stdout.clear();
            let response = DaemonResponse::error(id, "unavailable", "CA is not ready");
            assert_eq!(
                command
                    .render(response.clone(), &mut stdout, &mut stderr)
                    .unwrap(),
                1
            );
            assert!(stdout.is_empty());
            assert_eq!(
                serde_json::from_slice::<Value>(&stderr).unwrap(),
                serde_json::to_value(response).unwrap()
            );
        }
    }

    #[test]
    fn ca_rejects_malformed_success_even_when_selecting_the_other_format() {
        let command = Arguments::try_parse_from(["gateway", "ca", "--format", "pem"])
            .unwrap()
            .command;
        for result in [
            Value::Null,
            serde_json::json!({}),
            serde_json::json!({"caBundlePaths": null}),
            serde_json::json!({"caBundlePaths": {"pem": "/run/ca.pem"}}),
            serde_json::json!({"caBundlePaths": {"pem": "/run/ca.pem", "pkcs12": null}}),
            serde_json::json!({"caBundlePaths": {"pem": "/run/ca.pem", "pkcs12": 12}}),
            serde_json::json!({"caBundlePaths": {"pem": "/run/ca.pem", "pkcs12": "relative.p12"}}),
            serde_json::json!({"caBundlePaths": {"pem": "", "pkcs12": "/run/ca.p12"}}),
            serde_json::json!({"caBundlePaths": {"pem": "/run/ca.pem", "pkcs12": "/run/ca\0.p12"}}),
            serde_json::json!({"caBundlePaths": {"pem": "/run/ca.pem", "pkcs12": "/run/ca.p12", "privateKey": "/secret"}}),
            serde_json::json!({"caBundlePaths": {"pem": "/run/ca.pem", "pkcs12": "/run/ca.p12"}, "extra": true}),
        ] {
            let response = DaemonResponse::success(
                asc_daemon_protocol::RequestId::new("ca-test").unwrap(),
                result,
            );
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            assert_eq!(
                command
                    .render(response, &mut stdout, &mut stderr)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData
            );
            assert!(stdout.is_empty() && stderr.is_empty());
        }
    }
}
