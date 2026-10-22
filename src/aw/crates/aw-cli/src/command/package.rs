//! Typed local-package and offline-edit options, independent of runtime state.

use clap::Args;

#[derive(Args, Debug, PartialEq)]
#[group(multiple = true)]
pub(super) struct Install {
    /// Absolute destination for a local Preview bundle
    #[arg(long, value_name = "ABS_DIR", requires = "bundle", required_unless_present = "config", conflicts_with_all = ["config", "agent", "native_profile"])]
    pub prefix: Option<String>,
    /// Absolute unpacked Preview bundle directory; no URL or RPM/DEB backend
    #[arg(long, value_name = "ABS_DIR", requires = "prefix", conflicts_with_all = ["config", "agent", "native_profile"])]
    pub bundle: Option<String>,
    /// Legacy native Hook connection; prefer aw agent connect
    #[arg(long, value_name = "FILE", requires = "agent", conflicts_with_all = ["json", "quiet", "verbose", "color"])]
    pub config: Option<String>,
    /// Legacy native target; does not install Agent software
    #[arg(long, value_name = "TARGET", requires = "config")]
    pub agent: Option<String>,
    /// Native framework profile for legacy Hook connection
    #[arg(long, value_name = "NAME", requires_all = ["config", "agent"])]
    pub native_profile: Option<String>,
    #[command(flatten)]
    pub output: crate::output::Options,
}

#[derive(Args, Debug, PartialEq)]
pub(super) struct Prefix {
    /// Absolute immutable Preview installation prefix
    #[arg(long, value_name = "ABS_DIR")]
    pub prefix: String,
    #[command(flatten)]
    pub output: crate::output::Options,
}

#[derive(Args, Debug, PartialEq)]
pub(super) struct Uninstall {
    #[command(flatten)]
    pub prefix: Prefix,
    /// Confirm this removal; does not override ownership, integrity or permissions
    #[arg(short = 'y', long)]
    pub yes: bool,
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
    /// Absolute Qoder executable
    #[arg(long, value_name = "ABS_FILE", required_unless_present_any = ["openclaw", "qwenpaw"])]
    pub qoder: Option<String>,
    /// Absolute QwenPaw executable; generates the headless HTTP App target
    #[arg(long, value_name = "ABS_FILE", required_unless_present_any = ["qoder", "openclaw"])]
    pub qwenpaw: Option<String>,
    /// Absolute Node executable, paired with --openclaw
    #[arg(long, value_name = "ABS_FILE", requires = "openclaw")]
    pub node: Option<String>,
    /// Absolute OpenClaw entrypoint, paired with --node
    #[arg(
        long,
        value_name = "ABS_FILE",
        requires = "node",
        required_unless_present_any = ["qoder", "qwenpaw"]
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
    #[command(flatten)]
    pub output: crate::output::Options,
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
    #[command(flatten)]
    pub output: crate::output::Options,
}

#[derive(Args, Debug, PartialEq)]
pub(super) struct RemoveHook {
    #[command(flatten)]
    pub configuration: Configuration,
    #[arg(long, value_name = "EVENT")]
    pub event: String,
    #[arg(long, value_name = "ID", allow_hyphen_values = true)]
    pub id: String,
    #[command(flatten)]
    pub output: crate::output::Options,
}

/// The definition form retains its existing event/step semantics.
#[derive(Args, Debug, PartialEq)]
pub(super) struct AddHook {
    #[command(flatten)]
    pub configuration: Configuration,
    #[arg(long, value_name = "EVENT")]
    pub event: String,
    /// Complete step object; conflicts with command-generation options
    #[arg(long, value_name = "FILE", conflicts_with_all = ["id", "check", "provider", "effect", "on_error", "reason_code", "arguments"])]
    pub definition: Option<String>,
    /// Event-scoped Hook ID
    #[arg(
        long,
        value_name = "ID",
        required_unless_present = "definition",
        allow_hyphen_values = true
    )]
    pub id: Option<String>,
    /// Absolute script emitting a JSON boolean; never executed by the editor
    #[arg(
        long,
        value_name = "ABS_PROGRAM",
        required_unless_present = "definition"
    )]
    pub check: Option<String>,
    /// Provider name; defaults to the Hook ID, shared across events
    #[arg(long, value_name = "NAME", allow_hyphen_values = true)]
    pub provider: Option<String>,
    /// Effect requested on true; post-tool Hooks accept observe only
    #[arg(long, value_parser = ["block", "observe"], required_unless_present = "definition")]
    pub effect: Option<String>,
    /// Failure action, constrained by the event; never implicit
    #[arg(long, value_parser = ["block", "report", "withhold_result"], required_unless_present = "definition")]
    pub on_error: Option<String>,
    /// Optional machine-readable match reason
    #[arg(long, value_name = "CODE", allow_hyphen_values = true)]
    pub reason_code: Option<String>,
    /// Literal script arguments following --
    #[arg(last = true, value_name = "CHECK_ARGS")]
    pub arguments: Vec<String>,
    #[command(flatten)]
    pub output: crate::output::Options,
}
