//! Typed local-package and offline-edit options, independent of runtime state.

use clap::Args;

#[derive(Args, Debug, PartialEq)]
#[group(required = true, multiple = true)]
pub(super) struct Install {
    /// Absolute destination for a local Preview bundle
    #[arg(long, value_name = "ABS_DIR", requires = "bundle", conflicts_with_all = ["config", "agent", "native_profile"])]
    pub prefix: Option<String>,
    /// Absolute unpacked Preview bundle directory; no URL or RPM/DEB backend
    #[arg(long, value_name = "ABS_DIR", requires = "prefix", conflicts_with_all = ["config", "agent", "native_profile"])]
    pub bundle: Option<String>,
    /// Legacy native Hook connection; prefer aw agent connect
    #[arg(long, value_name = "FILE", requires = "agent")]
    pub config: Option<String>,
    /// Legacy native target; does not install Agent software
    #[arg(long, value_name = "TARGET", requires = "config")]
    pub agent: Option<String>,
    /// Native framework profile for legacy Hook connection
    #[arg(long, value_name = "NAME", requires_all = ["config", "agent"])]
    pub native_profile: Option<String>,
}

#[derive(Args, Debug, PartialEq)]
pub(super) struct Prefix {
    /// Absolute immutable Preview installation prefix
    #[arg(long, value_name = "ABS_DIR")]
    pub prefix: String,
}

#[derive(Args, Debug, PartialEq)]
pub(super) struct Configure {
    #[command(flatten)]
    pub prefix: Prefix,
    /// New absolute private configuration file outside the prefix
    #[arg(long, value_name = "ABS_FILE")]
    pub config: String,
    /// Absolute private state directory outside the prefix
    #[arg(long, value_name = "ABS_DIR")]
    pub state_dir: String,
    /// Absolute Qoder executable; choose Qoder and/or OpenClaw
    #[arg(long, value_name = "ABS_FILE", required_unless_present = "openclaw")]
    pub qoder: Option<String>,
    /// Absolute Node executable, paired with --openclaw
    #[arg(long, value_name = "ABS_FILE", requires = "openclaw")]
    pub node: Option<String>,
    /// Absolute OpenClaw entrypoint, paired with --node
    #[arg(
        long,
        value_name = "ABS_FILE",
        requires = "node",
        required_unless_present = "qoder"
    )]
    pub openclaw: Option<String>,
    /// Provider template; none is the default
    #[arg(long, value_parser = ["none", "tokenless", "sec-core", "sec-core+tokenless", "command"])]
    pub provider: Option<String>,
    /// Absolute backend socket, only for the sec-core templates
    #[arg(long, value_name = "ABS_FILE")]
    pub socket: Option<String>,
    /// Absolute program for the command template; generation never executes it
    #[arg(long, value_name = "ABS_PROGRAM")]
    pub check: Option<String>,
    /// Effect for the command template
    #[arg(long, value_parser = ["block", "observe"])]
    pub effect: Option<String>,
    /// Reason code for the command template
    #[arg(long, value_name = "CODE", allow_hyphen_values = true)]
    pub reason_code: Option<String>,
    /// Literal command-template arguments following --
    #[arg(last = true, value_name = "CHECK_ARGS")]
    pub arguments: Vec<String>,
}

#[derive(Args, Debug, PartialEq)]
pub(super) struct Configuration {
    /// Absolute existing configuration file for the offline editor
    #[arg(long, value_name = "ABS_FILE")]
    pub config: String,
}

#[derive(Args, Debug, PartialEq)]
pub(super) struct Show {
    #[command(flatten)]
    pub configuration: Configuration,
    /// Select one Provider, including private parameters
    #[arg(
        long,
        value_name = "NAME",
        conflicts_with = "event",
        allow_hyphen_values = true
    )]
    pub provider: Option<String>,
    /// Select one event
    #[arg(long, value_name = "EVENT")]
    pub event: Option<String>,
    /// Select one Hook within --event
    #[arg(
        long,
        value_name = "ID",
        requires = "event",
        allow_hyphen_values = true
    )]
    pub id: Option<String>,
}

#[derive(Args, Debug, PartialEq)]
pub(super) struct RemoveProvider {
    #[command(flatten)]
    pub configuration: Configuration,
    /// Exact Provider name
    #[arg(long, value_name = "NAME", allow_hyphen_values = true)]
    pub name: String,
}

#[derive(Args, Debug, PartialEq)]
pub(super) struct Edit {
    #[command(flatten)]
    pub provider: RemoveProvider,
    /// Complete YAML/JSON object; never executed
    #[arg(long, value_name = "FILE")]
    pub definition: String,
}

#[derive(Args, Debug, PartialEq)]
pub(super) struct Event {
    #[command(flatten)]
    pub configuration: Configuration,
    #[arg(long, value_name = "EVENT")]
    pub event: String,
    /// Complete YAML/JSON object; never executed
    #[arg(long, value_name = "FILE")]
    pub definition: String,
}

#[derive(Args, Debug, PartialEq)]
pub(super) struct RemoveHook {
    #[command(flatten)]
    pub configuration: Configuration,
    #[arg(long, value_name = "EVENT")]
    pub event: String,
    #[arg(long, value_name = "ID", allow_hyphen_values = true)]
    pub id: String,
}
