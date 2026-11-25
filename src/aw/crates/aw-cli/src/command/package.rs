//! Typed Provider-extension and offline-edit options, independent of runtime state.

use clap::Args;

#[derive(Args, Debug, PartialEq)]
pub(super) struct Install {
    /// Absolute installation directory for a local bundle
    #[arg(long, value_name = "ABS_DIR")]
    pub prefix: String,
    /// Absolute unpacked bundle directory; no URL or RPM/DEB backend
    #[arg(long, value_name = "ABS_DIR")]
    pub bundle: String,
    /// Show packages, files and conflicts without creating or changing anything
    #[arg(long)]
    pub dry_run: bool,
    /// Package database scope (default: user); system writes require root
    #[arg(long, value_enum)]
    pub scope: Option<super::scope::Scope>,
    #[command(flatten)]
    pub output: crate::output::Options,
}

#[derive(Args, Debug, PartialEq)]
pub(super) struct Prefix {
    /// Absolute software installation directory
    #[arg(long, value_name = "ABS_DIR")]
    pub prefix: String,
    #[command(flatten)]
    pub output: crate::output::Options,
}

#[derive(Args, Debug, PartialEq)]
pub(super) struct Uninstall {
    /// Package IDs to remove; omit to remove the entire installation
    #[arg(value_name = "PACKAGE")]
    pub packages: Vec<String>,
    /// Retain changed or missing selected files while removing their package records
    #[arg(long)]
    pub keep_changed: bool,
    #[command(flatten)]
    pub prefix: Prefix,
    /// Show removal and blockers without changing files or asking for confirmation
    #[arg(long)]
    pub dry_run: bool,
    /// Package database scope (default: user); system writes require root
    #[arg(long, value_enum)]
    pub scope: Option<super::scope::Scope>,
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
    #[arg(long, value_parser = ["none", "command"], conflicts_with = "template")]
    pub provider: Option<String>,
    /// Explicit distributor or Provider configuration template
    #[arg(long, value_name = "ABS_FILE")]
    pub template: Option<String>,
    /// Literal template variable NAME=VALUE; PREFIX and STATE are supplied by AW
    #[arg(long, value_name = "NAME=VALUE", requires = "template")]
    pub template_var: Vec<String>,
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
    /// Show SDK settings for the Adapter's unique or named target
    #[arg(long, value_name = "ADAPTER[/TARGET]", conflicts_with_all = ["event", "provider", "id"])]
    pub sdk_target: Option<String>,
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

#[derive(Args, Debug, PartialEq)]
pub(super) struct NativeSources {
    #[command(flatten)]
    pub configuration: Configuration,
    /// Existing AW Agent target
    #[arg(long, value_name = "TARGET")]
    pub agent: String,
}

#[derive(Args, Debug, PartialEq)]
pub(super) struct AddNativeSource {
    #[command(flatten)]
    pub target: NativeSources,
    /// AW source descriptor; never the native configuration contents
    #[arg(long, value_name = "FILE")]
    pub definition: String,
    #[command(flatten)]
    pub output: crate::output::Options,
}

#[derive(Args, Debug, PartialEq)]
pub(super) struct RemoveNativeSource {
    #[command(flatten)]
    pub target: NativeSources,
    /// Exact AW source descriptor ID
    #[arg(long, value_name = "ID", allow_hyphen_values = true)]
    pub id: String,
    #[command(flatten)]
    pub output: crate::output::Options,
}

pub(super) fn show_target(path: &str, selection: &str) -> crate::cli::Result<crate::cli::Exit> {
    let (adapter, target) = selection
        .split_once('/')
        .map_or((selection, None), |(adapter, target)| {
            (adapter, Some(target))
        });
    let config = aw_package::config_edit::read_configuration(std::path::Path::new(path))?;
    let spec = &config.as_value()["spec"];
    let agents = spec["agents"]
        .as_object()
        .ok_or(aw_config::Error::InvalidSchema)?;
    let mut matches = agents.iter().filter(|(name, agent)| {
        agent["adapter"] == adapter && target.is_none_or(|target| target == name.as_str())
    });
    let view = match (matches.next(), matches.next()) {
        (Some((name, agent)), None) => {
            let requirements = aw_config::requirements::project(&config, name)?;
            let mut view = serde_json::json!({
                "target": name,
                "daemon": {"endpoint": spec["daemon"]["endpoint"], "state_dir": spec["daemon"]["state_dir"]},
                "input_enabled": requirements.iter().any(|event| event.event == "input.submit"),
                "target_requires_service": requirements.iter().any(|event| event.required),
            });
            let hosts = aw_config::plaintext_http_hosts(agent);
            if !hosts.is_empty() {
                view["allow_plaintext_http_hosts"] = serde_json::json!(hosts);
            }
            if aw_adapter::deployment::emits_session_start_hint(adapter)
                && requirements
                    .iter()
                    .any(|event| event.event == "session.start")
            {
                view["observe_session_start"] = serde_json::json!(true);
            }
            view
        }
        _ => serde_json::json!({"target": null}),
    };
    serde_json::to_writer(std::io::stdout().lock(), &view)?;
    Ok(crate::cli::Exit::Code(0))
}
