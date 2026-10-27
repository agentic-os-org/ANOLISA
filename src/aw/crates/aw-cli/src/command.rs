//! One typed declaration defines the public command tree, options and help.

mod confirmation;
mod discovery;
mod dispatch;
mod inventory;
mod management;
mod package;
mod scope;
#[cfg(test)]
mod tests;
mod validation;

use clap::{Args, Parser, Subcommand};
use package::{Configure, Edit, Install, Show};

#[derive(Parser, Debug, PartialEq)]
#[command(
    name = "aw",
    version,
    about = "Run native Agents and manage local AW software and configuration",
    disable_help_subcommand = true,
    arg_required_else_help = true,
    after_help = "Install accepts local unpacked bundles with manifest.json or package.json.\nCompatibility: aw install --config FILE --agent TARGET connects native Hooks;\naw serve/status/stop and aw validate remain available.\nOptions are command-local; --json is not a global output mode."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug, PartialEq)]
enum Command {
    /// Print a shell completion script; does not install it
    Completion {
        /// Shell receiving the generated script
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// List implemented public commands, not runtime or Provider capabilities
    Commands {
        /// Emit versioned metadata from the same declaration as parsing and help
        #[arg(long)]
        json: bool,
    },
    /// Launch a configured Agent; preserve its output, exit code and signals
    Run(Run),
    /// Install a local software bundle, or retain legacy native Hook connection
    Install(Install),
    /// Remove selected packages or an entire installation, preserving unknown files
    Uninstall(package::Uninstall),
    /// List recorded software installations without checking or changing their files
    List(inventory::List),
    /// Check installed software files without repairing or starting anything
    Doctor(inventory::Query),
    /// Recover interrupted package operations from their durable local records
    Recover(management::Recover),
    /// Connect a configured Agent's persistent native Hooks
    #[command(subcommand)]
    Agent(Agent),
    /// Generate, inspect, validate or edit offline configuration
    #[command(subcommand)]
    Config(Config),
    /// Run or query the local service; does not install a system service
    #[command(
        subcommand,
        after_help = "No system service is installed. serve runs in the foreground."
    )]
    Service(Service),
    /// Update a supported idle instance's runtime policy
    Update(Update),
    /// Query service and optional Provider views; may execute view commands
    Inspect(Inspect),
    /// Send one operation from stdin JSON to an existing service
    Request(Request),
    /// Serve the command Provider stdin/stdout protocol
    Policy,
    /// Compatibility alias for service serve
    #[command(hide = true)]
    Serve(Serve),
    /// Compatibility alias for service status
    #[command(hide = true)]
    Status(Endpoint),
    /// Compatibility alias for service stop
    #[command(hide = true)]
    Stop(Endpoint),
    /// Compatibility alias for config validate
    #[command(hide = true)]
    Validate(Validate),
    /// Internal native callback protocol
    #[command(hide = true)]
    Hook(Hook),
    /// Internal reloadable native lifecycle protocol
    #[command(hide = true)]
    Bridge(Bridge),
    /// Internal resident SDK task lifecycle protocol
    #[command(hide = true)]
    Scope(Scope),
}

#[derive(Subcommand, Debug, PartialEq)]
enum Agent {
    /// Connect supported native Hooks; does not install Agent software
    Connect(Connect),
}

#[derive(Subcommand, Debug, PartialEq)]
enum Service {
    /// Run in the foreground without installing a system service
    Serve(Serve),
    /// Return the existing service's JSON status without starting it
    Status(Endpoint),
    /// Request graceful shutdown and retain the existing JSON response
    Stop(Endpoint),
}

#[derive(Subcommand, Debug, PartialEq)]
enum Config {
    /// Create a configuration from installed Preview packages
    Init(Configure),
    /// Print configuration JSON, including private Provider values
    Show(Show),
    /// Check configuration; optionally compare a policy update's scope
    Validate(ManagedValidate),
    /// Add a complete Provider definition without starting a service
    AddProvider(Edit),
    /// Remove an unreferenced Provider definition
    RemoveProvider(package::RemoveProvider),
    /// Add a complete event definition
    AddEvent(package::Event),
    /// Append a complete Hook step to an existing event
    AddHook(package::Event),
    /// Remove a Hook step; retain the event even when empty
    RemoveHook(package::RemoveHook),
}

#[derive(Args, Debug, PartialEq)]
struct Native {
    /// Configuration document; relative paths retain legacy behavior
    #[arg(long, value_name = "FILE")]
    config: String,
    /// Target name declared in the configuration
    #[arg(long, value_name = "TARGET")]
    agent: String,
    /// Native framework profile, not an AW deployment profile
    #[arg(long, value_name = "NAME")]
    native_profile: Option<String>,
}

#[derive(Args, Debug, PartialEq)]
struct Connect {
    #[command(flatten)]
    native: Native,
    #[command(flatten)]
    attachment: Attachment,
}

#[derive(Args, Debug, PartialEq)]
#[group(multiple = true)]
struct Attachment {
    /// Attach to an idle caller-owned Qoder TUI or QwenPaw App process
    #[arg(long, value_name = "PID", value_parser = clap::value_parser!(u32).range(1..), requires_all = ["config", "agent"], conflicts_with = "native_profile")]
    pid: Option<u32>,
    /// Detach owned Hooks while idle; leave the native process running
    #[arg(long, requires = "pid")]
    remove: bool,
}

#[derive(Args, Debug, PartialEq)]
struct Run {
    #[command(flatten)]
    native: Native,
    /// Opt in to the supported stable policy-update bridge
    #[arg(long)]
    reloadable: bool,
    /// Native framework settings file
    #[arg(long, value_name = "FILE")]
    native_settings: Option<String>,
    /// Native framework state directory
    #[arg(long, value_name = "DIR")]
    native_state_dir: Option<String>,
    /// Literal Agent arguments; use -- before the first argument
    #[arg(last = true, value_name = "AGENT_ARGS")]
    arguments: Vec<String>,
}

#[derive(Args, Debug, PartialEq)]
#[group(id = "endpoint", required = true, multiple = false)]
struct Endpoint {
    /// Original launch configuration used to locate the service
    #[arg(long, value_name = "FILE")]
    config: Option<String>,
    /// Explicit local service socket
    #[arg(long, value_name = "ABS_PATH")]
    socket: Option<String>,
}

#[derive(Args, Debug, PartialEq)]
struct Serve {
    /// Configuration document
    #[arg(long, value_name = "FILE")]
    config: String,
    /// Absolute private service state directory
    #[arg(long, value_name = "ABS_DIR")]
    state_dir: String,
}

#[derive(Args, Debug, PartialEq)]
struct Validate {
    /// Candidate configuration document
    #[arg(long, value_name = "FILE")]
    config: String,
    /// Baseline for policy-only scope comparison; does not activate changes
    #[arg(long, value_name = "BASELINE_FILE")]
    against: Option<String>,
}

#[derive(Args, Debug, PartialEq)]
struct ManagedValidate {
    #[command(flatten)]
    validation: Validate,
    #[command(flatten)]
    output: crate::output::Options,
}

#[derive(Args, Debug, PartialEq)]
struct Update {
    #[command(flatten)]
    endpoint: Endpoint,
    /// Select a configured target; combine with --instance to disambiguate
    #[arg(long, value_name = "TARGET", required_unless_present = "instance")]
    agent: Option<String>,
    /// Select an instance, optionally intersecting --agent
    #[arg(long, value_name = "ID", required_unless_present = "agent")]
    instance: Option<String>,
    /// Separate candidate; defaults to --config for an existing connection
    #[arg(long, value_name = "FILE", required_unless_present = "config")]
    policy: Option<String>,
    /// Optimistic concurrency precondition; no automatic retry on conflict
    #[arg(long, value_name = "SHA256")]
    expected_revision: Option<String>,
}

#[derive(Args, Debug, PartialEq)]
struct Inspect {
    /// Absolute configuration file for service and Provider view queries
    #[arg(long, value_name = "ABS_FILE")]
    config: String,
    /// Required legacy JSON output; never a global option
    #[arg(long, required = true)]
    json: bool,
}

#[derive(Args, Debug, PartialEq)]
struct Request {
    /// Existing service socket; does not start a service
    #[arg(long, value_name = "ABS_PATH")]
    socket: String,
    /// RPC deadline in milliseconds (default: 5000)
    #[arg(long, value_name = "1..60000", value_parser = clap::value_parser!(u64).range(1..=60000))]
    timeout_ms: Option<u64>,
}

#[derive(Args, Debug, PartialEq)]
struct Bridge {
    #[arg(long)]
    adapter: String,
    #[arg(long, value_name = "FILE")]
    binding: String,
}

#[derive(Args, Debug, PartialEq)]
struct Hook {
    #[command(flatten)]
    bridge: Bridge,
    #[arg(long)]
    event: String,
    #[arg(long)]
    step: String,
    #[arg(long)]
    on_error: String,
    #[arg(long, value_parser = ["true"])]
    close_event: Option<String>,
}

#[derive(Args, Debug, PartialEq)]
struct Scope {
    #[command(flatten)]
    bridge: Bridge,
    #[arg(long, value_parser = ["open", "acknowledge", "close", "invoke"])]
    action: String,
    #[arg(long)]
    event: Option<String>,
    #[arg(long)]
    step: Option<String>,
    #[arg(long)]
    on_error: Option<String>,
}

pub(crate) fn run(arguments: Vec<String>) -> crate::cli::Result<crate::cli::Exit> {
    if let Some(protocol) = arguments
        .first()
        .filter(|name| matches!(name.as_str(), "hook" | "bridge" | "scope"))
    {
        // Native callbacks have fail-closed replies and exit codes even for
        // malformed arguments. Keep their exact parser and protocol boundary.
        return crate::runtime(
            protocol,
            &crate::cli::Arguments::parse_protocol(arguments[1..].iter().cloned())?,
        );
    }
    let parsed = Cli::try_parse_from(std::iter::once("aw".to_owned()).chain(arguments.clone()));
    match parsed {
        Ok(cli) => dispatch::run(cli.command),
        Err(error) => {
            use clap::error::ErrorKind;
            match error.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
                    // Clap stops at a display flag. Do not silently discard later
                    // arguments, or allow duplicate display flags to hide errors.
                    let expected: &[&str] = if error.kind() == ErrorKind::DisplayVersion {
                        &["--version", "-V"]
                    } else {
                        &["--help", "-h"]
                    };
                    let queries = arguments
                        .iter()
                        .filter(|value| expected.contains(&value.as_str()))
                        .count();
                    if queries != 1
                        || !arguments
                            .last()
                            .is_some_and(|value| expected.contains(&value.as_str()))
                    {
                        return Err(
                            "help or version must be the final option and cannot be repeated"
                                .into(),
                        );
                    }
                    print!("{error}");
                    Ok(crate::cli::Exit::Code(0))
                }
                ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
                    print!("{error}");
                    Ok(crate::cli::Exit::Code(0))
                }
                _ => Err(error.into()),
            }
        }
    }
}
