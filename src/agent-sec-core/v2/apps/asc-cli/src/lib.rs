//! Command parsing and input preparation, separate from transport and rendering.

pub mod capabilities;
mod commands;
pub mod output;

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use asc_daemon_protocol::DaemonRequest;
use asc_foundation_types::{DAEMON_SOCKET_ENV, daemon_socket_path_from_env};
use clap::Parser;
use commands::Command;
pub use commands::{CapabilitiesCommand, PiiOutputFormat};

/// Parsed invocation for one CLI command.
#[derive(Debug)]
pub struct Cli {
    /// Absolute endpoint of an already-running daemon; absent for local commands.
    socket: Option<PathBuf>,
    timeout_ms: u32,
    command: Command,
    trace_context: Option<serde_json::Map<String, serde_json::Value>>,
}

/// How a parsed invocation reaches its result.
#[derive(Debug)]
pub enum Plan<'a> {
    /// Rendered from the process environment without any daemon involvement.
    Local(&'a CapabilitiesCommand),
    /// Sent to the daemon listening on `socket`.
    Daemon {
        /// Resolved absolute endpoint of the running daemon.
        socket: &'a Path,
    },
}

#[derive(Debug, Parser)]
#[command(
    name = "agent-sec-cli",
    version,
    about = "Manage Policy, Scope and Binding through asc-daemon"
)]
struct Arguments {
    /// V1 business correlation JSON for scan-pii; place before the command.
    #[arg(long)]
    trace_context: Option<String>,
    /// Absolute endpoint; otherwise `AGENT_SEC_DAEMON_SOCKET` or the system default.
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    /// Total connect/write/read deadline in milliseconds; requests are never retried.
    #[arg(long, global = true, default_value_t = 5000, value_parser = clap::value_parser!(u32).range(1..))]
    timeout_ms: u32,
    #[command(subcommand)]
    command: Command,
}

impl Cli {
    /// Parses argv including its executable name, preserving OS-native file paths.
    ///
    /// # Errors
    /// Returns clap help/version outcomes or usage errors; never connects to a daemon.
    pub fn parse_from<I, T>(arguments: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let socket_env = std::env::var_os(DAEMON_SOCKET_ENV);
        Self::parse_from_with_socket_env(arguments, socket_env.as_deref())
    }

    fn parse_from_with_socket_env<I, T>(
        arguments: I,
        socket_env: Option<&OsStr>,
    ) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let argv: Vec<OsString> = arguments.into_iter().map(Into::into).collect();
        let arguments = Arguments::try_parse_from(&argv)?;
        // Clap propagates global values across subcommands using last-wins.
        // Scan text/code accept hyphen values, so their next token is data even
        // when it happens to spell a global option.
        for option in ["--socket", "--timeout-ms"] {
            let mut tokens = argv.iter().skip(1);
            let mut count = 0;
            while let Some(argument) = tokens.next() {
                if argument == "--text" || argument == "--code" {
                    tokens.next();
                } else if argument.as_bytes().split(|byte| *byte == b'=').next()
                    == Some(option.as_bytes())
                {
                    count += 1;
                }
            }
            if count > 1 {
                return Err(clap::Error::raw(
                    clap::error::ErrorKind::ArgumentConflict,
                    format!("{option} may be specified only once"),
                ));
            }
        }
        let trace_context = parse_trace_context(arguments.trace_context.as_deref())?;
        if arguments.trace_context.is_some() && arguments.command.pii_format().is_none() {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::ArgumentConflict,
                "--trace-context is currently supported by scan-pii",
            ));
        }
        let socket = match arguments.command.local() {
            // A local command must stay usable on hosts that never deploy a
            // daemon, so an absent or malformed endpoint is not an error here.
            Some(_) => None,
            None => Some(resolve_socket(arguments.socket, socket_env)?),
        };
        Ok(Self {
            socket,
            timeout_ms: arguments.timeout_ms,
            command: arguments.command,
            trace_context,
        })
    }

    /// Returns the daemon endpoint, or `None` for a locally rendered command.
    pub fn socket(&self) -> Option<&Path> {
        self.socket.as_deref()
    }

    /// Reports whether this invocation runs locally or against the daemon.
    pub fn plan(&self) -> Plan<'_> {
        match (self.command.local(), self.socket.as_deref()) {
            (Some(command), _) => Plan::Local(command),
            (None, Some(socket)) => Plan::Daemon { socket },
            // Parsing rejects a daemon command without an endpoint, so the
            // remaining combination cannot be constructed.
            (None, None) => unreachable!("daemon commands always carry an endpoint"),
        }
    }

    /// Returns the single call deadline duration.
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(u64::from(self.timeout_ms))
    }

    /// Delegates to the selected command to construct a typed daemon request.
    ///
    /// # Errors
    /// Returns a file read, template decode, or request encoding error.
    pub fn request(&self) -> Result<DaemonRequest, InputError> {
        let mut request = self.command.request()?;
        if let Some(trace) = &self.trace_context {
            request.params["traceContext"] = serde_json::Value::Object(trace.clone());
        }
        Ok(request)
    }

    /// Whether this invocation uses the V1-compatible scan-code projection.
    pub const fn is_scan_code(&self) -> bool {
        self.command.is_scan_code()
    }

    /// Selected PII presentation, or `None` for another command.
    pub const fn pii_format(&self) -> Option<PiiOutputFormat> {
        self.command.pii_format()
    }
}

fn parse_trace_context(
    value: Option<&str>,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, clap::Error> {
    let Some(value) = value.filter(|s| !s.trim().is_empty()) else {
        return Ok(None);
    };
    let payload: serde_json::Value = serde_json::from_str(value).map_err(|_| {
        clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            "invalid trace context JSON",
        )
    })?;
    let Some(payload) = payload.as_object() else {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            "trace context must be a JSON object",
        ));
    };
    Ok(Some(
        asc_action_types::ActionTraceContext::from_payload(Some(payload)).to_payload(),
    ))
}

/// Resolves the daemon endpoint from the option, then the environment.
fn resolve_socket(
    option: Option<PathBuf>,
    socket_env: Option<&OsStr>,
) -> Result<PathBuf, clap::Error> {
    let socket = match option {
        Some(socket) => socket,
        None => daemon_socket_path_from_env(
            socket_env
                .filter(|path| !path.is_empty())
                .or(Some(OsStr::new("/run/agent-sec-core/daemon.sock"))),
        )
        .map_err(|error| {
            clap::Error::raw(clap::error::ErrorKind::ValueValidation, error.to_string())
        })?,
    };
    if socket.is_absolute() {
        Ok(socket)
    } else {
        Err(clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            "--socket must be an absolute path",
        ))
    }
}

/// Local input failures, reported as execution failures rather than daemon errors.
#[derive(Debug, thiserror::Error)]
pub enum InputError {
    /// Local PII file or stdin access failed.
    #[error("cannot read PII input: {0}")]
    PiiRead(#[source] std::io::Error),
    /// Malformed UTF-8 is never silently repaired.
    #[error("PII input must be valid UTF-8")]
    PiiUtf8,
    /// Untruncated input cannot fit the daemon transport.
    #[error("PII input exceeds the 4 MiB transport frame limit")]
    PiiTooLarge,
    /// The V1-compatible scan-code command received no non-whitespace source.
    #[error("Error: --code is required (use --code '<source>')")]
    EmptyCode,
    /// Template file access failed.
    #[error("cannot read Policy template: {0}")]
    Read(#[from] std::io::Error),
    /// Bound input before parsing or constructing a request.
    #[error("Policy template exceeds the 4194304-byte input limit")]
    TooLarge,
    /// Invalid authoring JSON or request serialization.
    #[error("invalid Policy request input: {0}")]
    Json(#[from] serde_json::Error),
    /// A locally rendered command was asked for a daemon request.
    #[error("this command is rendered locally and sends no daemon request")]
    LocalCommand,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanner_hyphen_values_are_data_but_duplicate_global_options_fail() {
        for (command, input) in [("scan-pii", "--text"), ("scan-code", "--code")] {
            for literal in ["--socket", "--timeout-ms", "--socket=/literal"] {
                let cli = Cli::parse_from_with_socket_env(
                    [
                        "agent-sec-cli",
                        "--socket",
                        "/run/asc.sock",
                        command,
                        input,
                        literal,
                    ],
                    None,
                )
                .unwrap();
                let key = if command == "scan-pii" {
                    "text"
                } else {
                    "code"
                };
                assert_eq!(cli.request().unwrap().params[key], literal);
            }
        }
        let error = Cli::parse_from_with_socket_env(
            [
                "agent-sec-cli",
                "--socket",
                "/run/one.sock",
                "scan-pii",
                "--text",
                "",
                "--socket",
                "/run/two.sock",
            ],
            None,
        )
        .unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn environment_socket_is_used_when_the_option_is_omitted() {
        let cli = Cli::parse_from_with_socket_env(
            ["agent-sec-cli", "scan-code", "--code", "echo hello"],
            Some(OsStr::new("/run/custom/daemon.sock")),
        )
        .expect("deployment endpoint parses");
        assert_eq!(cli.socket(), Some(Path::new("/run/custom/daemon.sock")));
        assert!(matches!(cli.plan(), Plan::Daemon { .. }));
    }

    #[test]
    fn explicit_socket_overrides_the_environment_socket() {
        let cli = Cli::parse_from_with_socket_env(
            [
                "agent-sec-cli",
                "--socket",
                "/run/explicit.sock",
                "scan-code",
                "--code",
                "echo hello",
            ],
            Some(OsStr::new("/run/agent-sec-core/daemon.sock")),
        )
        .expect("explicit endpoint parses");
        assert_eq!(cli.socket(), Some(Path::new("/run/explicit.sock")));
    }

    #[test]
    fn relative_environment_socket_is_a_usage_error() {
        let error = Cli::parse_from_with_socket_env(
            ["agent-sec-cli", "scan-code", "--code", "echo hello"],
            Some(OsStr::new("relative")),
        )
        .expect_err("invalid deployment endpoint must fail before connecting");
        assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn absent_or_empty_environment_socket_uses_system_default() {
        for socket_env in [None, Some(OsStr::new(""))] {
            let cli = Cli::parse_from_with_socket_env(
                ["agent-sec-cli", "scan-code", "--code", "echo hello"],
                socket_env,
            )
            .expect("system endpoint parses");
            assert_eq!(
                cli.socket(),
                Some(Path::new("/run/agent-sec-core/daemon.sock"))
            );
        }
    }

    #[test]
    fn the_capability_view_parses_without_any_deployment_endpoint() {
        for socket_env in [None, Some(OsStr::new("relative")), Some(OsStr::new(""))] {
            let cli =
                Cli::parse_from_with_socket_env(["agent-sec-cli", "capabilities"], socket_env)
                    .expect("the capability view never needs a daemon");
            assert_eq!(cli.socket(), None);
            assert!(matches!(cli.plan(), Plan::Local(_)));
            assert!(matches!(cli.request(), Err(InputError::LocalCommand)));
        }
    }
}
