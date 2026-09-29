//! `anolisa system` command surface — daemon lifecycle management.
//!
//! Subcommands:
//! - `serve` — start the system-helper daemon (foreground, for systemd).
//! - `setup` — one-time installation of the system helper daemon.
//! - `teardown` — remove system helper: stop service, delete unit + binary.
//! - `status` — check system helper health (read-only, no root required).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use clap::{Parser, Subcommand};
use serde::Serialize;

use anolisa_core::daemon_server::DaemonServer;
use anolisa_platform::command::{CommandRunner, InheritedLocaleCommandRunner};
use anolisa_platform::fs_layout::FsLayout;
use anolisa_platform::ipc::SYSTEM_HELPER_SOCKET;
use anolisa_platform::privilege;
use anolisa_platform::systemd::{Systemd, SystemdError};

use crate::context::CliContext;
use crate::helper_client::{HandshakeResult, HelperClient, HelperClientError, HelperStatus};
use crate::response::{self, CliError};

#[derive(Parser)]
pub struct SystemArgs {
    #[command(subcommand)]
    pub command: SystemCommands,
}

#[derive(Subcommand)]
pub enum SystemCommands {
    /// Start the system helper daemon (foreground, for systemd)
    Serve {
        /// Socket path override
        #[arg(long, default_value = SYSTEM_HELPER_SOCKET)]
        socket: String,
    },
    /// One-time setup: install system helper daemon
    Setup {
        /// Override helper binary destination (defaults to FsLayout libexec_dir)
        #[arg(long)]
        helper_path: Option<String>,

        /// Upgrade existing installation
        #[arg(long)]
        upgrade: bool,
    },
    /// Remove system helper: stop service, delete unit + binary
    Teardown,
    /// Check system helper health
    Status {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
}

pub fn handle(args: SystemArgs, ctx: &CliContext) -> Result<(), CliError> {
    match args.command {
        SystemCommands::Serve { socket } => handle_serve(&socket),
        SystemCommands::Setup {
            helper_path,
            upgrade,
        } => handle_setup(helper_path.as_deref(), upgrade, ctx),
        SystemCommands::Teardown => handle_teardown(ctx),
        SystemCommands::Status { json } => handle_status(json, ctx),
    }
}

fn handle_serve(socket: &str) -> Result<(), CliError> {
    require_root(
        "system serve",
        "the system helper daemon must run as root (euid 0)",
        "run with sudo or as a systemd service",
    )?;

    let server = DaemonServer::new(socket);
    server.run().map_err(|e| CliError::Runtime {
        command: "system serve".to_string(),
        reason: format!("daemon exited with error: {e}"),
    })
}

// ─── Setup ───────────────────────────────────────────────────────────────────

const SERVICE_NAME: &str = "anolisa-system-helper";
const UNIT_FILENAME: &str = "anolisa-system-helper.service";
const RUNTIME_DIR: &str = "/run/anolisa";
const ANOLISA_GROUP: &str = "anolisa";

/// Resolve the system-mode FsLayout from context.
fn resolve_layout(ctx: &CliContext) -> FsLayout {
    ctx.visible_system_layout().clone()
}

fn require_root(command: &str, reason: &str, hint: &str) -> Result<(), CliError> {
    if privilege::is_root() {
        return Ok(());
    }

    Err(CliError::PermissionDenied {
        command: command.to_string(),
        reason: reason.to_string(),
        hint: Some(hint.to_string()),
    })
}

fn handle_setup(
    helper_path_override: Option<&str>,
    upgrade: bool,
    ctx: &CliContext,
) -> Result<(), CliError> {
    let cmd = "system setup";

    require_root(
        cmd,
        "system setup must be run as root (euid 0)",
        "run with: sudo anolisa system setup",
    )?;

    let layout = resolve_layout(ctx);
    let helper_path: PathBuf = match helper_path_override {
        Some(p) => PathBuf::from(p),
        None => layout.libexec_dir.join("anolisa-system-helper"),
    };
    let unit_path = layout.systemd_unit_dir.join(UNIT_FILENAME);
    let systemd = Systemd::system();

    // 2. Stop the service if it's running (avoids "Text file busy" on binary overwrite)
    stop_service_before_setup(&systemd);

    // 3. Copy current exe to helper_path
    let current_exe = std::env::current_exe().map_err(|e| CliError::Runtime {
        command: cmd.to_string(),
        reason: format!("failed to determine current executable path: {e}"),
    })?;

    // Ensure parent directory exists
    if let Some(parent) = helper_path.parent() {
        fs::create_dir_all(parent).map_err(|e| CliError::Runtime {
            command: cmd.to_string(),
            reason: format!("failed to create directory {}: {e}", parent.display()),
        })?;
    }

    fs::copy(&current_exe, &helper_path).map_err(|e| CliError::Runtime {
        command: cmd.to_string(),
        reason: format!("failed to copy binary to {}: {e}", helper_path.display()),
    })?;
    eprintln!(
        "[setup] installed helper binary → {}",
        helper_path.display()
    );

    // 4. Set helper permissions (0755)
    fs::set_permissions(&helper_path, fs::Permissions::from_mode(0o755)).map_err(|e| {
        CliError::Runtime {
            command: cmd.to_string(),
            reason: format!(
                "failed to set permissions on {}: {e}",
                helper_path.display()
            ),
        }
    })?;

    setup_access_with(
        cmd,
        upgrade,
        &InheritedLocaleCommandRunner,
        || std::env::var("SUDO_USER"),
        RUNTIME_DIR,
    )?;

    // 8. Generate systemd unit file
    write_unit_file(cmd, &helper_path, &unit_path)?;

    // 9. Deploy sandbox.toml configuration file
    deploy_sandbox_config(cmd, &layout)?;

    // 10. systemctl daemon-reload + enable + start/restart
    reload_and_start_service(cmd, upgrade, &systemd)?;

    // 11. Verify socket
    verify_socket(cmd)?;

    // 12. Success
    eprintln!("[setup] anolisa system helper is running and verified.");
    Ok(())
}

fn setup_access_with<R, F>(
    cmd: &str,
    upgrade: bool,
    runner: &R,
    read_sudo_user: F,
    runtime_dir: &str,
) -> Result<(), CliError>
where
    R: CommandRunner,
    F: FnOnce() -> Result<String, std::env::VarError>,
{
    if !upgrade {
        setup_group(cmd, runner)?;
        setup_user_membership(cmd, runner, read_sudo_user)?;
    }
    setup_runtime_dir(cmd, runner, runtime_dir)
}

fn setup_group<R: CommandRunner>(cmd: &str, runner: &R) -> Result<(), CliError> {
    let output = runner
        .run("groupadd", &["-r", ANOLISA_GROUP])
        .map_err(|e| CliError::Runtime {
            command: cmd.to_string(),
            reason: format!("failed to run groupadd: {e}"),
        })?;

    // Exit code 9 means group already exists — not an error.
    if !matches!(output.code, Some(0 | 9)) {
        let stderr = output.stderr;
        return Err(CliError::Runtime {
            command: cmd.to_string(),
            reason: format!("groupadd -r {ANOLISA_GROUP} failed: {stderr}"),
        });
    }
    eprintln!("[setup] system group '{ANOLISA_GROUP}' ensured");
    Ok(())
}

fn setup_user_membership<R, F>(cmd: &str, runner: &R, read_sudo_user: F) -> Result<(), CliError>
where
    R: CommandRunner,
    F: FnOnce() -> Result<String, std::env::VarError>,
{
    let user = read_sudo_user().unwrap_or_default();
    if user.is_empty() {
        eprintln!("[setup] warning: $SUDO_USER not set, skipping group membership");
        return Ok(());
    }

    let output = runner
        .run("usermod", &["-aG", ANOLISA_GROUP, &user])
        .map_err(|e| CliError::Runtime {
            command: cmd.to_string(),
            reason: format!("failed to run usermod: {e}"),
        })?;

    if output.code != Some(0) {
        let stderr = output.stderr;
        return Err(CliError::Runtime {
            command: cmd.to_string(),
            reason: format!("usermod -aG {ANOLISA_GROUP} {user} failed: {stderr}"),
        });
    }
    eprintln!("[setup] user '{user}' added to group '{ANOLISA_GROUP}'");
    Ok(())
}

fn setup_runtime_dir<R: CommandRunner>(
    cmd: &str,
    runner: &R,
    runtime_dir: &str,
) -> Result<(), CliError> {
    fs::create_dir_all(runtime_dir).map_err(|e| CliError::Runtime {
        command: cmd.to_string(),
        reason: format!("failed to create {runtime_dir}: {e}"),
    })?;

    let output = runner
        .run("chgrp", &[ANOLISA_GROUP, runtime_dir])
        .map_err(|e| CliError::Runtime {
            command: cmd.to_string(),
            reason: format!("failed to run chgrp: {e}"),
        })?;
    if output.code != Some(0) {
        let stderr = output.stderr;
        return Err(CliError::Runtime {
            command: cmd.to_string(),
            reason: format!("chgrp {ANOLISA_GROUP} {runtime_dir} failed: {stderr}"),
        });
    }

    fs::set_permissions(runtime_dir, fs::Permissions::from_mode(0o750)).map_err(|e| {
        CliError::Runtime {
            command: cmd.to_string(),
            reason: format!("failed to chmod {runtime_dir}: {e}"),
        }
    })?;
    eprintln!("[setup] runtime directory {runtime_dir} ready");
    Ok(())
}

/// Determine the sandbox.toml deployment path.
///
/// - System-level (euid==0): `<layout.etc_dir>/sandbox.toml`
/// - User-level: `$XDG_CONFIG_HOME/anolisa/sandbox.toml`
fn resolve_sandbox_config_path(layout: &FsLayout) -> PathBuf {
    if privilege::is_root() {
        layout.etc_dir.join("sandbox.toml")
    } else {
        let config_home = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
            format!("{home}/.config")
        });
        PathBuf::from(config_home)
            .join("anolisa")
            .join("sandbox.toml")
    }
}

fn deploy_sandbox_config(cmd: &str, layout: &FsLayout) -> Result<(), CliError> {
    const SANDBOX_TOML_TEMPLATE: &str = include_str!("../../../../manifests/sandbox.toml");

    let config_path = resolve_sandbox_config_path(layout);

    if config_path.exists() {
        eprintln!(
            "[setup] sandbox.toml already exists, skipping (remove the file manually to regenerate)"
        );
        return Ok(());
    }

    // Ensure parent directory exists
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).map_err(|e| CliError::Runtime {
            command: cmd.to_string(),
            reason: format!("failed to create directory {}: {e}", parent.display()),
        })?;
    }

    fs::write(&config_path, SANDBOX_TOML_TEMPLATE).map_err(|e| CliError::Runtime {
        command: cmd.to_string(),
        reason: format!(
            "failed to write sandbox.toml to {}: {e}",
            config_path.display()
        ),
    })?;

    eprintln!(
        "[setup] sandbox.toml deployed \u{2192} {}",
        config_path.display()
    );
    Ok(())
}

fn write_unit_file(cmd: &str, helper_path: &Path, unit_path: &Path) -> Result<(), CliError> {
    const UNIT_TEMPLATE: &str =
        include_str!("../../../../systemd/anolisa-system-helper.service.in");

    let unit_content = UNIT_TEMPLATE
        .replace("@@HELPER_PATH@@", &helper_path.display().to_string())
        .replace("@@SOCKET_PATH@@", SYSTEM_HELPER_SOCKET);

    // Ensure unit directory exists
    if let Some(parent) = unit_path.parent() {
        fs::create_dir_all(parent).map_err(|e| CliError::Runtime {
            command: cmd.to_string(),
            reason: format!("failed to create directory {}: {e}", parent.display()),
        })?;
    }

    fs::write(unit_path, &unit_content).map_err(|e| CliError::Runtime {
        command: cmd.to_string(),
        reason: format!("failed to write unit file {}: {e}", unit_path.display()),
    })?;
    eprintln!("[setup] systemd unit written → {}", unit_path.display());
    Ok(())
}

fn reload_and_start_service<R: CommandRunner>(
    cmd: &str,
    upgrade: bool,
    systemd: &Systemd<R>,
) -> Result<(), CliError> {
    systemd
        .daemon_reload()
        .map_err(|error| systemd_cli_error(cmd, &["daemon-reload"], error))?;
    systemd
        .enable_unit_file(SERVICE_NAME)
        .map_err(|error| systemd_cli_error(cmd, &["enable", SERVICE_NAME], error))?;

    if upgrade {
        systemd
            .restart_unit(SERVICE_NAME)
            .map_err(|error| systemd_cli_error(cmd, &["restart", SERVICE_NAME], error))?;
    } else {
        systemd
            .start_unit(SERVICE_NAME)
            .map_err(|error| systemd_cli_error(cmd, &["start", SERVICE_NAME], error))?;
    }
    eprintln!("[setup] service {SERVICE_NAME} active");
    Ok(())
}

fn stop_service_before_setup<R: CommandRunner>(systemd: &Systemd<R>) {
    let _ = systemd.stop_unit(SERVICE_NAME);
}

fn systemd_cli_error(cmd: &str, args: &[&str], error: SystemdError) -> CliError {
    let reason = match error {
        SystemdError::Spawn { source, .. } => {
            format!("failed to run systemctl {}: {source}", args.join(" "))
        }
        SystemdError::NonZeroExit(failure) => {
            format!("systemctl {} failed: {}", args.join(" "), failure.stderr)
        }
        SystemdError::NotFound(unit) => {
            format!(
                "systemctl {} failed: service not found: {unit}",
                args.join(" ")
            )
        }
    };
    CliError::Runtime {
        command: cmd.to_string(),
        reason,
    }
}

fn verify_socket(cmd: &str) -> Result<(), CliError> {
    let socket_path = Path::new(SYSTEM_HELPER_SOCKET);
    verify_socket_with(
        cmd,
        || socket_path.exists(),
        thread::sleep,
        || HelperClient::connect(socket_path),
    )
}

fn verify_socket_with<P, S, C>(
    cmd: &str,
    mut socket_exists: P,
    mut sleep: S,
    connect: C,
) -> Result<(), CliError>
where
    P: FnMut() -> bool,
    S: FnMut(Duration),
    C: FnOnce() -> Result<HelperClient, HelperClientError>,
{
    // Wait briefly for the socket to appear (daemon may take a moment to start).
    let mut attempts = 0;
    while !socket_exists() && attempts < 10 {
        sleep(Duration::from_millis(300));
        attempts += 1;
    }

    if !socket_exists() {
        return Err(CliError::Runtime {
            command: cmd.to_string(),
            reason: format!("socket {SYSTEM_HELPER_SOCKET} did not appear within 3 seconds"),
        });
    }

    verify_helper_connection(cmd, connect)
}

fn verify_helper_connection<F>(cmd: &str, connect: F) -> Result<(), CliError>
where
    F: FnOnce() -> Result<HelperClient, HelperClientError>,
{
    let mut client = connect().map_err(|error| CliError::Runtime {
        command: cmd.to_string(),
        reason: verify_connection_error(error),
    })?;
    let handshake = client
        .handshake(env!("CARGO_PKG_VERSION"))
        .map_err(|error| CliError::Runtime {
            command: cmd.to_string(),
            reason: verify_connection_error(error),
        })?;
    if !handshake.compatible {
        return Err(CliError::Runtime {
            command: cmd.to_string(),
            reason: "handshake succeeded but version is incompatible".to_string(),
        });
    }
    eprintln!("[setup] handshake verified — helper is operational");
    Ok(())
}

fn verify_connection_error(error: HelperClientError) -> String {
    match error {
        HelperClientError::Connect { path, source } => {
            format!("failed to connect to {}: {source}", path.display())
        }
        HelperClientError::Send { source, .. } => format!("handshake send failed: {source}"),
        HelperClientError::Receive { source, .. } => format!("handshake recv failed: {source}"),
        HelperClientError::Remote { code, message, .. } => format!(
            "unexpected handshake response: Error {{ code: {code:?}, message: {message:?} }}"
        ),
        HelperClientError::UnexpectedResponse { response, .. } => {
            format!("unexpected handshake response: {response:?}")
        }
    }
}

// ─── Teardown ────────────────────────────────────────────────────────────────

fn handle_teardown(ctx: &CliContext) -> Result<(), CliError> {
    let cmd = "system teardown";

    require_root(
        cmd,
        "system teardown must be run as root (euid 0)",
        "run with: sudo anolisa system teardown",
    )?;

    let layout = resolve_layout(ctx);
    let helper_path = layout.libexec_dir.join("anolisa-system-helper");
    let unit_path = layout.systemd_unit_dir.join(UNIT_FILENAME);
    let mut warnings: Vec<String> = Vec::new();
    let systemd = Systemd::system();

    // 2-3. Stop and disable service while retaining failures as warnings.
    stop_and_disable_service(cmd, &systemd, &mut warnings);

    // 4. Delete unit file
    if unit_path.exists() {
        if let Err(e) = fs::remove_file(&unit_path) {
            warnings.push(format!(
                "failed to remove unit file {}: {e}",
                unit_path.display()
            ));
        } else {
            eprintln!("[teardown] removed unit file {}", unit_path.display());
        }
    } else {
        warnings.push(format!("unit file {} already removed", unit_path.display()));
    }

    // 5. Reload systemd
    reload_systemd_after_teardown(cmd, &systemd, &mut warnings);

    // 6. Delete helper binary
    if helper_path.exists() {
        if let Err(e) = fs::remove_file(&helper_path) {
            warnings.push(format!(
                "failed to remove helper binary {}: {e}",
                helper_path.display()
            ));
        } else {
            eprintln!("[teardown] removed helper binary {}", helper_path.display());
        }
    } else {
        warnings.push(format!(
            "helper binary {} already removed",
            helper_path.display()
        ));
    }

    // 7. Remove sandbox.toml config file
    let sandbox_config_path = resolve_sandbox_config_path(&layout);
    if sandbox_config_path.exists() {
        if let Err(e) = fs::remove_file(&sandbox_config_path) {
            warnings.push(format!(
                "failed to remove sandbox.toml {}: {e}",
                sandbox_config_path.display()
            ));
        } else {
            eprintln!("[teardown] removed sandbox.toml");
        }
    }

    // 8. Optionally remove /run/anolisa/
    let runtime_path = Path::new(RUNTIME_DIR);
    if runtime_path.exists() {
        if let Err(e) = fs::remove_dir_all(runtime_path) {
            warnings.push(format!("failed to remove {RUNTIME_DIR}: {e}"));
        } else {
            eprintln!("[teardown] removed runtime directory {RUNTIME_DIR}");
        }
    }

    // 9. Print warnings and success
    for w in &warnings {
        eprintln!("[teardown] warning: {w}");
    }
    eprintln!("[teardown] system helper teardown complete.");
    Ok(())
}

fn stop_and_disable_service<R: CommandRunner>(
    cmd: &str,
    systemd: &Systemd<R>,
    warnings: &mut Vec<String>,
) {
    match systemd.stop_unit(SERVICE_NAME) {
        Ok(()) => eprintln!("[teardown] stopped {SERVICE_NAME}"),
        Err(SystemdError::NotFound(_)) => {
            warnings.push(format!(
                "service {SERVICE_NAME} was not loaded (already stopped)"
            ));
        }
        Err(error @ SystemdError::NonZeroExit(_)) => {
            if matches!(
                systemd.unit_status(SERVICE_NAME),
                Err(SystemdError::NotFound(_))
            ) {
                warnings.push(format!(
                    "service {SERVICE_NAME} was not loaded (already stopped)"
                ));
            } else {
                let error = systemd_cli_error(cmd, &["stop", SERVICE_NAME], error);
                warnings.push(format!("failed to stop {SERVICE_NAME}: {error}"));
            }
        }
        Err(error) => {
            let error = systemd_cli_error(cmd, &["stop", SERVICE_NAME], error);
            warnings.push(format!("failed to stop {SERVICE_NAME}: {error}"));
        }
    }

    match systemd.disable_unit_file(SERVICE_NAME) {
        Ok(()) => eprintln!("[teardown] disabled {SERVICE_NAME}"),
        Err(error) => {
            let error = systemd_cli_error(cmd, &["disable", SERVICE_NAME], error);
            warnings.push(format!("failed to disable {SERVICE_NAME}: {error}"));
        }
    }
}

fn reload_systemd_after_teardown<R: CommandRunner>(
    cmd: &str,
    systemd: &Systemd<R>,
    warnings: &mut Vec<String>,
) {
    if let Err(error) = systemd.daemon_reload() {
        let error = systemd_cli_error(cmd, &["daemon-reload"], error);
        warnings.push(format!("daemon-reload failed: {error}"));
    } else {
        eprintln!("[teardown] systemd daemon-reload complete");
    }
}

// ─── Status command ─────────────────────────────────────────────────────────────────

const STATUS_SERVICE_UNIT: &str = "anolisa-system-helper.service";

/// JSON output payload for `system status --json`.
#[derive(Debug, Serialize)]
struct StatusReport {
    service_active: bool,
    socket_exists: bool,
    socket_connectable: bool,
    helper_version: Option<String>,
    cli_version: String,
    version_compatible: bool,
    uptime_secs: Option<u64>,
    last_operation: Option<String>,
    last_operation_time: Option<String>,
}

struct SystemStatusObservation {
    service_state: StatusServiceState,
    report: StatusReport,
}

fn handle_status(json: bool, ctx: &CliContext) -> Result<(), CliError> {
    let observation = collect_status_with(
        env!("CARGO_PKG_VERSION"),
        &Systemd::system(),
        || Path::new(SYSTEM_HELPER_SOCKET).exists(),
        || HelperClient::connect(Path::new(SYSTEM_HELPER_SOCKET)),
    );
    render_status(json, ctx, observation)
}

fn collect_status_with<R, F, C>(
    cli_version: &str,
    systemd: &Systemd<R>,
    socket_exists: F,
    connect: C,
) -> SystemStatusObservation
where
    R: CommandRunner,
    F: FnOnce() -> bool,
    C: FnOnce() -> Result<HelperClient, HelperClientError>,
{
    let service_state = check_service_state(systemd);
    let socket_exists = socket_exists();

    let connection = if socket_exists {
        try_status_connection_with(cli_version, connect)
    } else {
        HelperConnectionStatus::disconnected()
    };

    let helper_version = connection
        .handshake
        .as_ref()
        .map(|handshake| handshake.helper_version.clone());
    let version_compatible = connection
        .handshake
        .as_ref()
        .map(|handshake| handshake.compatible)
        .unwrap_or(false);

    let uptime_secs = connection.status.as_ref().map(|status| status.uptime_secs);
    let last_operation = connection
        .status
        .as_ref()
        .and_then(|status| status.last_operation.clone());
    let last_operation_time = connection
        .status
        .as_ref()
        .and_then(|status| status.last_operation_time.clone());

    let report = StatusReport {
        service_active: service_state == StatusServiceState::Active,
        socket_exists,
        socket_connectable: connection.connectable,
        helper_version,
        cli_version: cli_version.to_string(),
        version_compatible,
        uptime_secs,
        last_operation,
        last_operation_time,
    };

    SystemStatusObservation {
        service_state,
        report,
    }
}

fn render_status(
    json: bool,
    ctx: &CliContext,
    observation: SystemStatusObservation,
) -> Result<(), CliError> {
    let SystemStatusObservation {
        service_state,
        report,
    } = observation;
    if json || ctx.json {
        return response::render_json("system status", report);
    }

    // Human-readable output.
    print_status_human(
        &service_state,
        report.socket_exists,
        report.socket_connectable,
        report.helper_version.as_deref(),
        &report.cli_version,
        report.version_compatible,
        report.uptime_secs,
        report.last_operation.as_deref(),
        report.last_operation_time.as_deref(),
    );

    Ok(())
}

// ─── Status helpers ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusServiceState {
    Active,
    Inactive,
    Failed,
    NotInstalled,
    Unknown,
}

impl StatusServiceState {
    fn label(self) -> &'static str {
        match self {
            Self::Active => "active (running)",
            Self::Inactive => "inactive (stopped)",
            Self::Failed => "failed",
            Self::NotInstalled => "not installed",
            Self::Unknown => "unknown",
        }
    }
}

fn check_service_state<R: CommandRunner>(systemd: &Systemd<R>) -> StatusServiceState {
    match systemd.unit_status(STATUS_SERVICE_UNIT) {
        Ok(status) => {
            if status.failed {
                StatusServiceState::Failed
            } else if status.active {
                StatusServiceState::Active
            } else {
                StatusServiceState::Inactive
            }
        }
        Err(SystemdError::NotFound(_)) => StatusServiceState::NotInstalled,
        Err(_) => StatusServiceState::Unknown,
    }
}

#[derive(Debug)]
struct HelperConnectionStatus {
    connectable: bool,
    handshake: Option<HandshakeResult>,
    status: Option<HelperStatus>,
}

impl HelperConnectionStatus {
    fn disconnected() -> Self {
        Self {
            connectable: false,
            handshake: None,
            status: None,
        }
    }
}

/// Attempt to connect to the helper socket, perform handshake, and query
/// system status while retaining partial typed evidence.
fn try_status_connection_with<F>(cli_version: &str, connect: F) -> HelperConnectionStatus
where
    F: FnOnce() -> Result<HelperClient, HelperClientError>,
{
    let mut client = match connect() {
        Ok(client) => client,
        Err(_) => return HelperConnectionStatus::disconnected(),
    };

    let handshake = match client.handshake(cli_version) {
        Ok(handshake) => handshake,
        Err(_) => {
            return HelperConnectionStatus {
                connectable: true,
                handshake: None,
                status: None,
            };
        }
    };

    let status = if handshake.compatible {
        client.system_status().ok()
    } else {
        None
    };
    HelperConnectionStatus {
        connectable: true,
        handshake: Some(handshake),
        status,
    }
}

#[allow(clippy::too_many_arguments)]
fn print_status_human(
    service_state: &StatusServiceState,
    socket_exists: bool,
    socket_connectable: bool,
    helper_version: Option<&str>,
    cli_version: &str,
    version_compatible: bool,
    uptime_secs: Option<u64>,
    last_operation: Option<&str>,
    last_operation_time: Option<&str>,
) {
    println!("anolisa system helper:");
    println!("  Status:      {}", service_state.label());

    let socket_label = if socket_connectable {
        format!("{SYSTEM_HELPER_SOCKET} [connected]")
    } else if socket_exists {
        format!("{SYSTEM_HELPER_SOCKET} [not connectable]")
    } else {
        format!("{SYSTEM_HELPER_SOCKET} [missing]")
    };
    println!("  Socket:      {socket_label}");

    if let Some(hv) = helper_version {
        let compat_mark = if version_compatible {
            "\u{2713}"
        } else {
            "\u{26a0} version mismatch"
        };
        println!("  Version:     {hv} (CLI: {cli_version}) {compat_mark}");
    }

    if let Some(secs) = uptime_secs {
        println!("  Uptime:      {}", format_status_uptime(secs));
    }

    if let Some(op) = last_operation {
        let time_suffix = last_operation_time
            .map(|t| format!(" ({t})"))
            .unwrap_or_default();
        println!("  Last op:     {op}{time_suffix}");
    }

    println!();
    if *service_state == StatusServiceState::NotInstalled || !socket_exists {
        println!("  hint: run 'sudo anolisa system setup' to install");
    } else if socket_connectable && version_compatible {
        println!("  All checks passed.");
    } else if !version_compatible && helper_version.is_some() {
        println!("  warning: CLI and helper versions differ; consider restarting the helper.");
    }
}

fn format_status_uptime(secs: u64) -> String {
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    if hours > 0 {
        format!("{hours}h {mins:02}m")
    } else {
        format!("{mins}m")
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::io;
    use std::rc::Rc;

    use anolisa_core::system_helper::{HelperRequest, HelperResponse};
    use anolisa_platform::command::{CommandOutput, CommandRunner};

    use super::*;
    use crate::helper_client::ScriptedTransport;

    enum FakeOutcome {
        Output(CommandOutput),
        Spawn(io::ErrorKind),
    }

    type FakeCalls = Rc<RefCell<VecDeque<(Vec<String>, FakeOutcome)>>>;

    struct FakeSystemdRunner {
        calls: FakeCalls,
    }

    impl CommandRunner for FakeSystemdRunner {
        fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
            assert_eq!(program, "systemctl");
            let (expected, outcome) = self
                .calls
                .borrow_mut()
                .pop_front()
                .expect("unexpected systemctl call");
            assert_eq!(args, expected);
            match outcome {
                FakeOutcome::Output(output) => Ok(output),
                FakeOutcome::Spawn(kind) => {
                    Err(io::Error::new(kind, "fake systemctl spawn failure"))
                }
            }
        }
    }

    fn fake_systemd(
        calls: Vec<(Vec<&str>, FakeOutcome)>,
    ) -> (Systemd<FakeSystemdRunner>, FakeCalls) {
        let calls = Rc::new(RefCell::new(
            calls
                .into_iter()
                .map(|(args, outcome)| (args.into_iter().map(str::to_string).collect(), outcome))
                .collect(),
        ));
        let runner = FakeSystemdRunner {
            calls: Rc::clone(&calls),
        };
        (Systemd::with_runner(runner), calls)
    }

    fn success() -> FakeOutcome {
        FakeOutcome::Output(CommandOutput {
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    fn non_zero(code: i32, stderr: &str) -> FakeOutcome {
        FakeOutcome::Output(CommandOutput {
            code: Some(code),
            stdout: String::new(),
            stderr: stderr.to_string(),
        })
    }

    fn loaded_status() -> FakeOutcome {
        FakeOutcome::Output(CommandOutput {
            code: Some(0),
            stdout: "LoadState=loaded\nActiveState=inactive\nUnitFileState=enabled\nDescription=ANOLISA\n"
                .to_string(),
            stderr: String::new(),
        })
    }

    fn missing_status() -> FakeOutcome {
        FakeOutcome::Output(CommandOutput {
            code: Some(0),
            stdout:
                "LoadState=not-found\nActiveState=inactive\nUnitFileState=\nDescription=missing\n"
                    .to_string(),
            stderr: String::new(),
        })
    }

    fn assert_systemd_finished(calls: &FakeCalls) {
        assert!(calls.borrow().is_empty());
    }

    struct FakeSetupRunner {
        calls: RefCell<Vec<Vec<String>>>,
        outcomes: RefCell<VecDeque<FakeOutcome>>,
        runtime_dir: PathBuf,
        remove_runtime_dir: bool,
    }

    impl FakeSetupRunner {
        fn new(runtime_dir: &Path, outcomes: Vec<FakeOutcome>) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                outcomes: RefCell::new(outcomes.into()),
                runtime_dir: runtime_dir.to_path_buf(),
                remove_runtime_dir: false,
            }
        }
    }

    impl CommandRunner for FakeSetupRunner {
        fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
            match program {
                "groupadd" | "usermod" => assert!(!self.runtime_dir.exists()),
                "chgrp" => {
                    assert_eq!(args, [ANOLISA_GROUP, self.runtime_dir.to_str().unwrap()]);
                    assert!(self.runtime_dir.is_dir());
                    if self.remove_runtime_dir {
                        // This runner owns only the test's dedicated empty directory.
                        fs::remove_dir(&self.runtime_dir).unwrap();
                    }
                }
                _ => panic!("unexpected setup program: {program}"),
            }
            self.calls.borrow_mut().push(
                std::iter::once(program)
                    .chain(args.iter().copied())
                    .map(str::to_string)
                    .collect(),
            );
            match self
                .outcomes
                .borrow_mut()
                .pop_front()
                .expect("unexpected setup call")
            {
                FakeOutcome::Output(output) => Ok(output),
                FakeOutcome::Spawn(kind) => Err(io::Error::new(kind, "fake setup spawn failure")),
            }
        }
    }

    #[test]
    fn setup_access_preserves_command_and_environment_order() {
        for group_code in [0, 9] {
            let tmp = tempfile::tempdir().unwrap();
            let runtime_dir = tmp.path().join("run with spaces/anolisa");
            let runtime = runtime_dir.to_str().unwrap();
            let runner = FakeSetupRunner::new(
                &runtime_dir,
                vec![non_zero(group_code, "ignored\n"), success(), success()],
            );
            let reads = std::cell::Cell::new(0);
            setup_access_with(
                "system setup",
                false,
                &runner,
                || {
                    reads.set(reads.get() + 1);
                    assert_eq!(
                        *runner.calls.borrow(),
                        vec![vec!["groupadd", "-r", ANOLISA_GROUP]]
                    );
                    assert!(!runtime_dir.exists());
                    Ok("alice".to_string())
                },
                runtime,
            )
            .unwrap();
            assert_eq!(reads.get(), 1);
            assert_eq!(
                *runner.calls.borrow(),
                vec![
                    vec!["groupadd", "-r", ANOLISA_GROUP],
                    vec!["usermod", "-aG", ANOLISA_GROUP, "alice"],
                    vec!["chgrp", ANOLISA_GROUP, runtime],
                ]
            );
            assert!(runner.outcomes.borrow().is_empty());
            assert_eq!(
                fs::metadata(&runtime_dir).unwrap().permissions().mode() & 0o777,
                0o750
            );
        }
    }

    #[test]
    fn setup_access_skips_unavailable_or_empty_user() {
        use std::os::unix::ffi::OsStringExt;

        for user in [
            Ok(String::new()),
            Err(std::env::VarError::NotPresent),
            Err(std::env::VarError::NotUnicode(
                std::ffi::OsString::from_vec(vec![0xff]),
            )),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let runtime_dir = tmp.path().join("run");
            let runtime = runtime_dir.to_str().unwrap();
            let runner = FakeSetupRunner::new(&runtime_dir, vec![success(), success()]);
            let reads = std::cell::Cell::new(0);
            setup_access_with(
                "system setup",
                false,
                &runner,
                || {
                    reads.set(reads.get() + 1);
                    assert_eq!(runner.calls.borrow().len(), 1);
                    user
                },
                runtime,
            )
            .unwrap();
            assert_eq!(reads.get(), 1);
            assert_eq!(
                *runner.calls.borrow(),
                vec![
                    vec!["groupadd", "-r", ANOLISA_GROUP],
                    vec!["chgrp", ANOLISA_GROUP, runtime],
                ]
            );
            assert!(runner.outcomes.borrow().is_empty());
            assert_eq!(
                fs::metadata(&runtime_dir).unwrap().permissions().mode() & 0o777,
                0o750
            );
        }
    }

    #[test]
    fn setup_access_upgrade_never_reads_user_or_manages_groups() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime_dir = tmp.path().join("run");
        let runtime = runtime_dir.to_str().unwrap();
        let runner = FakeSetupRunner::new(&runtime_dir, vec![success()]);
        setup_access_with(
            "system setup",
            true,
            &runner,
            || panic!("upgrade must not read SUDO_USER"),
            runtime,
        )
        .unwrap();
        assert_eq!(
            *runner.calls.borrow(),
            vec![vec!["chgrp", ANOLISA_GROUP, runtime]]
        );
        assert!(runner.outcomes.borrow().is_empty());
        assert_eq!(
            fs::metadata(&runtime_dir).unwrap().permissions().mode() & 0o777,
            0o750
        );
    }

    #[test]
    fn setup_access_command_failures_keep_diagnostics_and_stop_followups() {
        for stage in 0..3 {
            for (failure, stderr) in [
                (FakeOutcome::Spawn(io::ErrorKind::NotFound), None),
                (FakeOutcome::Spawn(io::ErrorKind::PermissionDenied), None),
                (non_zero(1, " \tdenied\n"), Some(" \tdenied\n")),
                (non_zero(2, ""), Some("")),
                (
                    FakeOutcome::Output(CommandOutput {
                        code: Some(3),
                        stdout: "stdout only".to_string(),
                        stderr: " \n".to_string(),
                    }),
                    Some(" \n"),
                ),
                (
                    FakeOutcome::Output(CommandOutput {
                        code: None,
                        stdout: "ignored".to_string(),
                        stderr: "killed\n".to_string(),
                    }),
                    Some("killed\n"),
                ),
                (
                    FakeOutcome::Output(CommandOutput {
                        code: None,
                        stdout: String::new(),
                        stderr: String::new(),
                    }),
                    Some(""),
                ),
            ] {
                let tmp = tempfile::tempdir().unwrap();
                let runtime_dir = tmp.path().join("run");
                let runtime = runtime_dir.to_str().unwrap();
                let mut outcomes: Vec<_> = (0..stage).map(|_| success()).collect();
                outcomes.push(failure);
                let runner = FakeSetupRunner::new(&runtime_dir, outcomes);
                let reads = std::cell::Cell::new(0);
                let error = setup_access_with(
                    "system setup",
                    false,
                    &runner,
                    || {
                        reads.set(reads.get() + 1);
                        assert_eq!(runner.calls.borrow().len(), 1);
                        Ok("alice".to_string())
                    },
                    runtime,
                )
                .unwrap_err();
                let expected = [
                    vec!["groupadd", "-r", ANOLISA_GROUP],
                    vec!["usermod", "-aG", ANOLISA_GROUP, "alice"],
                    vec!["chgrp", ANOLISA_GROUP, runtime],
                ];
                let reason = match stderr {
                    Some(stderr) => format!("{} failed: {stderr}", expected[stage].join(" ")),
                    None => format!(
                        "failed to run {}: fake setup spawn failure",
                        expected[stage][0]
                    ),
                };
                assert_eq!(error.command(), "system setup");
                assert_eq!(error.code(), "EXECUTION_FAILED");
                assert_eq!(error.exit_code(), 1);
                assert_eq!(error.reason(), reason);
                assert_eq!(error.to_string(), format!("execution failed: {reason}"));
                assert_eq!(*runner.calls.borrow(), expected[..=stage]);
                assert!(runner.outcomes.borrow().is_empty());
                assert_eq!(reads.get(), usize::from(stage > 0));
                assert_eq!(runtime_dir.exists(), stage == 2);
            }
        }
    }

    #[test]
    fn setup_access_mkdir_failure_never_calls_chgrp() {
        let tmp = tempfile::tempdir().unwrap();
        let blocker = tmp.path().join("file");
        fs::write(&blocker, "not a directory").unwrap();
        let runtime_dir = blocker.join("run");
        let runner = FakeSetupRunner::new(&runtime_dir, vec![]);
        let error = setup_access_with(
            "system setup",
            true,
            &runner,
            || panic!("upgrade must not read SUDO_USER"),
            runtime_dir.to_str().unwrap(),
        )
        .unwrap_err();
        assert!(
            error
                .reason()
                .starts_with(&format!("failed to create {}: ", runtime_dir.display()))
        );
        assert_eq!(error.code(), "EXECUTION_FAILED");
        assert_eq!(error.exit_code(), 1);
        assert!(runner.calls.borrow().is_empty());
        assert_eq!(fs::read_to_string(blocker).unwrap(), "not a directory");
    }

    #[test]
    fn setup_access_chgrp_failure_preserves_directory_permissions() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime_dir = tmp.path().join("run");
        fs::create_dir(&runtime_dir).unwrap();
        fs::set_permissions(&runtime_dir, fs::Permissions::from_mode(0o700)).unwrap();
        let runner = FakeSetupRunner::new(&runtime_dir, vec![non_zero(1, "denied")]);
        setup_access_with(
            "system setup",
            true,
            &runner,
            || panic!("upgrade must not read SUDO_USER"),
            runtime_dir.to_str().unwrap(),
        )
        .unwrap_err();
        assert_eq!(
            fs::metadata(&runtime_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(runner.outcomes.borrow().is_empty());
    }

    #[test]
    fn setup_access_chmod_failure_follows_chgrp() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime_dir = tmp.path().join("run");
        let runtime = runtime_dir.to_str().unwrap();
        let mut runner = FakeSetupRunner::new(&runtime_dir, vec![success()]);
        runner.remove_runtime_dir = true;
        let error = setup_access_with(
            "system setup",
            true,
            &runner,
            || panic!("upgrade must not read SUDO_USER"),
            runtime,
        )
        .unwrap_err();
        assert!(
            error
                .reason()
                .starts_with(&format!("failed to chmod {runtime}: "))
        );
        assert_eq!(error.command(), "system setup");
        assert_eq!(error.code(), "EXECUTION_FAILED");
        assert_eq!(error.exit_code(), 1);
        assert_eq!(
            *runner.calls.borrow(),
            vec![vec!["chgrp", ANOLISA_GROUP, runtime]]
        );
        assert!(runner.outcomes.borrow().is_empty());
        assert!(!runtime_dir.exists());
    }

    #[test]
    fn setup_access_preserves_progress_and_error_output() {
        let (_, module) = module_path!().split_once("::").unwrap();
        for scenario in ["normal", "missing", "upgrade", "failure"] {
            for mode in ["human", "json", "quiet"] {
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        format!("{module}::setup_access_output_child"),
                        "--exact".to_string(),
                        "--nocapture".to_string(),
                    ])
                    .env("ANOLISA_TEST_SETUP_ACCESS", scenario)
                    .env("ANOLISA_TEST_SETUP_OUTPUT", mode)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let stdout = String::from_utf8(output.stdout).unwrap();
                let runtime = stdout
                    .lines()
                    .find_map(|line| line.strip_prefix("SETUP_RUNTIME="))
                    .unwrap();
                let (_, rendered) = stdout.split_once("SETUP_OUTPUT_BEGIN\n").unwrap();
                let (rendered, _) = rendered.split_once("SETUP_OUTPUT_END\n").unwrap();
                let mut expected_stderr = String::new();
                if scenario != "upgrade" {
                    expected_stderr.push_str("[setup] system group 'anolisa' ensured\n");
                    expected_stderr.push_str(if scenario == "missing" {
                        "[setup] warning: $SUDO_USER not set, skipping group membership\n"
                    } else {
                        "[setup] user 'alice' added to group 'anolisa'\n"
                    });
                }
                if scenario == "failure" {
                    let reason = format!("chgrp anolisa {runtime} failed:  denied\n");
                    if mode == "json" {
                        let json: serde_json::Value = serde_json::from_str(rendered).unwrap();
                        assert_eq!(
                            json,
                            serde_json::json!({
                                "ok": false,
                                "schema_version": response::SCHEMA_VERSION,
                                "command": "system setup",
                                "warnings": [],
                                "error": {"code": "EXECUTION_FAILED", "reason": reason},
                            })
                        );
                    } else {
                        expected_stderr.push_str(&format!("error: {reason}\n"));
                        assert!(rendered.is_empty());
                    }
                } else {
                    expected_stderr
                        .push_str(&format!("[setup] runtime directory {runtime} ready\n"));
                    assert!(rendered.is_empty());
                }
                assert_eq!(String::from_utf8(output.stderr).unwrap(), expected_stderr);
                assert!(stdout.contains("test result: ok."));
            }
        }
    }

    #[test]
    fn setup_access_output_child() {
        let (_, module) = module_path!().split_once("::").unwrap();
        // Only the dedicated capture invocation may consume the scenario inputs.
        if std::env::args().skip(1).collect::<Vec<_>>()
            != [
                format!("{module}::setup_access_output_child"),
                "--exact".to_string(),
                "--nocapture".to_string(),
            ]
        {
            return;
        }
        let scenario = std::env::var("ANOLISA_TEST_SETUP_ACCESS").unwrap();
        let mode = std::env::var("ANOLISA_TEST_SETUP_OUTPUT").unwrap();
        let sandbox = crate::test_support::TestSandbox::new();
        let runtime_dir = sandbox.root().join("run");
        let runtime = runtime_dir.to_str().unwrap();
        let outcomes = match scenario.as_str() {
            "normal" => vec![success(), success(), success()],
            "missing" => vec![success(), success()],
            "upgrade" => vec![success()],
            "failure" => vec![success(), success(), non_zero(1, " denied\n")],
            _ => panic!("unknown setup fixture"),
        };
        let runner = FakeSetupRunner::new(&runtime_dir, outcomes);
        println!("SETUP_RUNTIME={runtime}");
        println!("SETUP_OUTPUT_BEGIN");
        let result = setup_access_with(
            "system setup",
            scenario == "upgrade",
            &runner,
            || {
                assert_ne!(scenario, "upgrade");
                if scenario == "missing" {
                    Err(std::env::VarError::NotPresent)
                } else {
                    Ok("alice".to_string())
                }
            },
            runtime,
        );
        if scenario == "failure" {
            let error = result.unwrap_err();
            let ctx = sandbox.context_with(
                crate::context::InstallMode::System,
                crate::test_support::TestContextOptions {
                    json: mode == "json",
                    quiet: mode == "quiet",
                    ..Default::default()
                },
            );
            assert_eq!(
                response::render_error(&ctx, &error),
                std::process::ExitCode::from(1)
            );
        } else {
            result.unwrap();
        }
        assert!(runner.outcomes.borrow().is_empty());
        println!("SETUP_OUTPUT_END");
    }

    #[test]
    fn setup_service_lifecycle_preserves_non_upgrade_order() {
        let (systemd, calls) = fake_systemd(vec![
            (vec!["daemon-reload"], success()),
            (vec!["enable", SERVICE_NAME], success()),
            (vec!["start", SERVICE_NAME], success()),
        ]);

        reload_and_start_service("system setup", false, &systemd)
            .expect("setup lifecycle should succeed");

        assert_systemd_finished(&calls);
    }

    #[test]
    fn setup_best_effort_stop_never_probes_status() {
        let cases = [
            success(),
            non_zero(5, "missing\n"),
            FakeOutcome::Spawn(io::ErrorKind::NotFound),
        ];

        for outcome in cases {
            let (systemd, calls) = fake_systemd(vec![(vec!["stop", SERVICE_NAME], outcome)]);

            stop_service_before_setup(&systemd);

            assert_systemd_finished(&calls);
        }
    }

    #[test]
    fn setup_service_lifecycle_restarts_during_upgrade() {
        let (systemd, calls) = fake_systemd(vec![
            (vec!["daemon-reload"], success()),
            (vec!["enable", SERVICE_NAME], success()),
            (vec!["restart", SERVICE_NAME], success()),
        ]);

        reload_and_start_service("system setup", true, &systemd)
            .expect("upgrade lifecycle should succeed");

        assert_systemd_finished(&calls);
    }

    #[test]
    fn setup_service_lifecycle_preserves_spawn_failure_message() {
        let (systemd, calls) = fake_systemd(vec![(
            vec!["daemon-reload"],
            FakeOutcome::Spawn(io::ErrorKind::PermissionDenied),
        )]);

        let error = reload_and_start_service("system setup", false, &systemd)
            .expect_err("spawn should fail setup");

        assert_eq!(
            error.reason(),
            "failed to run systemctl daemon-reload: fake systemctl spawn failure"
        );
        assert_systemd_finished(&calls);
    }

    #[test]
    fn setup_service_lifecycle_preserves_non_zero_exit_message() {
        let (systemd, calls) = fake_systemd(vec![
            (vec!["daemon-reload"], success()),
            (
                vec!["enable", SERVICE_NAME],
                non_zero(1, "enable refused\n"),
            ),
        ]);

        let error = reload_and_start_service("system setup", false, &systemd)
            .expect_err("enable should fail setup");

        assert_eq!(
            error.reason(),
            format!("systemctl enable {SERVICE_NAME} failed: enable refused\n")
        );
        assert_systemd_finished(&calls);
    }

    #[test]
    fn teardown_missing_unit_uses_typed_status_and_continues() {
        let (systemd, calls) = fake_systemd(vec![
            (
                vec!["stop", SERVICE_NAME],
                non_zero(5, "translated missing-unit diagnostic\n"),
            ),
            (
                vec![
                    "show",
                    SERVICE_NAME,
                    "--no-pager",
                    "--property=LoadState,ActiveState,UnitFileState,Description",
                ],
                missing_status(),
            ),
            (vec!["disable", SERVICE_NAME], success()),
        ]);
        let mut warnings = Vec::new();

        stop_and_disable_service("system teardown", &systemd, &mut warnings);

        assert_eq!(
            warnings,
            vec![format!(
                "service {SERVICE_NAME} was not loaded (already stopped)"
            )]
        );
        assert_systemd_finished(&calls);
    }

    #[test]
    fn teardown_preserves_stop_disable_reload_order() {
        let (systemd, calls) = fake_systemd(vec![
            (vec!["stop", SERVICE_NAME], success()),
            (vec!["disable", SERVICE_NAME], success()),
            (vec!["daemon-reload"], success()),
        ]);
        let mut warnings = Vec::new();

        stop_and_disable_service("system teardown", &systemd, &mut warnings);
        reload_systemd_after_teardown("system teardown", &systemd, &mut warnings);

        assert!(warnings.is_empty());
        assert_systemd_finished(&calls);
    }

    #[test]
    fn teardown_preserves_disable_and_reload_failure_warnings() {
        let (systemd, calls) = fake_systemd(vec![
            (vec!["stop", SERVICE_NAME], success()),
            (
                vec!["disable", SERVICE_NAME],
                non_zero(1, "disable refused\n"),
            ),
            (vec!["daemon-reload"], non_zero(1, "reload refused\n")),
        ]);
        let mut warnings = Vec::new();

        stop_and_disable_service("system teardown", &systemd, &mut warnings);
        reload_systemd_after_teardown("system teardown", &systemd, &mut warnings);

        assert_eq!(
            warnings,
            vec![
                format!(
                    "failed to disable {SERVICE_NAME}: execution failed: systemctl disable \
                     {SERVICE_NAME} failed: disable refused\n"
                ),
                "daemon-reload failed: execution failed: systemctl daemon-reload failed: \
                 reload refused\n"
                    .to_string(),
            ]
        );
        assert_systemd_finished(&calls);
    }

    #[test]
    fn teardown_non_missing_stop_failure_warns_and_disables() {
        let (systemd, calls) = fake_systemd(vec![
            (vec!["stop", SERVICE_NAME], non_zero(1, "access denied\n")),
            (
                vec![
                    "show",
                    SERVICE_NAME,
                    "--no-pager",
                    "--property=LoadState,ActiveState,UnitFileState,Description",
                ],
                loaded_status(),
            ),
            (vec!["disable", SERVICE_NAME], success()),
        ]);
        let mut warnings = Vec::new();

        stop_and_disable_service("system teardown", &systemd, &mut warnings);

        assert_eq!(
            warnings,
            vec![format!(
                "failed to stop {SERVICE_NAME}: execution failed: systemctl stop {SERVICE_NAME} failed: access denied\n"
            )]
        );
        assert_systemd_finished(&calls);
    }

    fn client_with_responses(responses: Vec<HelperResponse>) -> HelperClient {
        let (transport, _) =
            ScriptedTransport::new(Vec::new(), responses.into_iter().map(Ok).collect());
        HelperClient::with_transport(transport)
    }

    fn connect_error() -> HelperClientError {
        HelperClientError::Connect {
            path: PathBuf::from(SYSTEM_HELPER_SOCKET),
            source: io::Error::new(io::ErrorKind::ConnectionRefused, "not listening"),
        }
    }

    const STATUS_SERVICES: &[(&str, StatusServiceState, &str)] = &[
        ("active", StatusServiceState::Active, "active (running)"),
        ("reloading", StatusServiceState::Active, "active (running)"),
        (
            "inactive",
            StatusServiceState::Inactive,
            "inactive (stopped)",
        ),
        ("failed", StatusServiceState::Failed, "failed"),
        ("missing", StatusServiceState::NotInstalled, "not installed"),
        ("masked", StatusServiceState::NotInstalled, "not installed"),
        ("spawn", StatusServiceState::Unknown, "unknown"),
        ("non-zero", StatusServiceState::Unknown, "unknown"),
        ("signal", StatusServiceState::Unknown, "unknown"),
        ("empty", StatusServiceState::Inactive, "inactive (stopped)"),
    ];

    const STATUS_HELPERS: &[&str] = &[
        "missing-socket",
        "connect-failure",
        "handshake-send",
        "handshake-receive",
        "handshake-remote",
        "handshake-unexpected",
        "incompatible",
        "status-send",
        "status-receive",
        "status-remote",
        "status-unexpected",
        "complete",
        "minutes",
        "no-operation",
        "no-time",
    ];

    fn collect_status_fixture(service: &str, helper: &str) -> SystemStatusObservation {
        let service_result = match service {
            "active" | "reloading" | "inactive" | "failed" => FakeOutcome::Output(CommandOutput {
                code: Some(0),
                stdout: format!(
                    "LoadState=loaded\nActiveState={service}\nUnitFileState=enabled\nDescription=ANOLISA\n"
                ),
                stderr: String::new(),
            }),
            "missing" => missing_status(),
            "masked" => FakeOutcome::Output(CommandOutput {
                code: Some(0),
                stdout: "LoadState=masked\nActiveState=inactive\nUnitFileState=\n".to_string(),
                stderr: String::new(),
            }),
            "spawn" => FakeOutcome::Spawn(io::ErrorKind::NotFound),
            "non-zero" | "signal" => FakeOutcome::Output(CommandOutput {
                code: (service == "non-zero").then_some(1),
                // A failed process must not contribute seemingly valid properties.
                stdout: "LoadState=loaded\nActiveState=active\nUnitFileState=enabled\n".to_string(),
                stderr: "not installed".to_string(),
            }),
            "empty" => success(),
            _ => panic!("unknown service fixture: {service}"),
        };
        let (systemd, calls) = fake_systemd(vec![(
            vec![
                "show",
                STATUS_SERVICE_UNIT,
                "--no-pager",
                "--property=LoadState,ActiveState,UnitFileState,Description",
            ],
            service_result,
        )]);
        let compatible = || HelperResponse::HandshakeOk {
            helper_version: "0.3.9".to_string(),
            compatible: true,
        };
        let remote = || HelperResponse::Error {
            code: "UNAVAILABLE".to_string(),
            message: "probe unavailable".to_string(),
        };
        let unexpected = || HelperResponse::Success {
            message: "wrong response".to_string(),
            exit_code: 0,
        };
        let send_error = || Err(io::Error::new(io::ErrorKind::BrokenPipe, "send"));
        let receive_error = || Err(io::Error::new(io::ErrorKind::UnexpectedEof, "receive"));
        let (sends, receives, request_count) = match helper {
            "missing-socket" | "connect-failure" => (vec![], vec![], 0),
            "handshake-send" => (vec![send_error()], vec![], 1),
            "handshake-receive" => (vec![], vec![receive_error()], 1),
            "handshake-remote" => (vec![], vec![Ok(remote())], 1),
            "handshake-unexpected" => (vec![], vec![Ok(unexpected())], 1),
            "incompatible" => (
                vec![],
                vec![Ok(HelperResponse::HandshakeOk {
                    helper_version: "0.0.1".to_string(),
                    compatible: false,
                })],
                1,
            ),
            "status-send" => (vec![Ok(()), send_error()], vec![Ok(compatible())], 2),
            "status-receive" => (vec![], vec![Ok(compatible()), receive_error()], 2),
            "status-remote" => (vec![], vec![Ok(compatible()), Ok(remote())], 2),
            "status-unexpected" => (vec![], vec![Ok(compatible()), Ok(unexpected())], 2),
            "complete" | "minutes" | "no-operation" | "no-time" => (
                vec![],
                vec![
                    Ok(compatible()),
                    Ok(HelperResponse::Status {
                        // Deliberately disagree with systemd and the handshake.
                        running: service != "active",
                        version: "ignored-status-version".to_string(),
                        uptime_secs: if helper == "minutes" { 75 } else { 3720 },
                        last_operation: (helper != "no-operation").then(|| "install".to_string()),
                        last_operation_time: (helper != "no-time").then(|| "now".to_string()),
                    }),
                ],
                2,
            ),
            _ => panic!("unknown helper fixture: {helper}"),
        };
        let (transport, sent) = ScriptedTransport::new(sends, receives);
        let socket_calls = Cell::new(0);
        let connect_calls = Cell::new(0);
        let observation = collect_status_with(
            "0.3.2",
            &systemd,
            || {
                assert_systemd_finished(&calls);
                assert_eq!(connect_calls.get(), 0);
                assert!(sent.borrow().is_empty());
                socket_calls.set(socket_calls.get() + 1);
                helper != "missing-socket"
            },
            || {
                assert_systemd_finished(&calls);
                assert_eq!(socket_calls.get(), 1);
                assert!(sent.borrow().is_empty());
                connect_calls.set(connect_calls.get() + 1);
                assert_ne!(helper, "missing-socket", "connector must be skipped");
                if helper == "connect-failure" {
                    Err(connect_error())
                } else {
                    Ok(HelperClient::with_transport(transport))
                }
            },
        );
        assert_systemd_finished(&calls);
        assert_eq!(socket_calls.get(), 1);
        assert_eq!(connect_calls.get(), usize::from(helper != "missing-socket"));
        let expected_requests = [
            HelperRequest::Handshake {
                cli_version: "0.3.2".to_string(),
            },
            HelperRequest::SystemStatus,
        ];
        assert_eq!(*sent.borrow(), expected_requests[..request_count]);
        observation
    }

    fn expected_status_data(service_active: bool, helper: &str) -> serde_json::Value {
        let (connectable, version, compatible, uptime, operation, time) = match helper {
            "missing-socket" | "connect-failure" => (false, None, false, None, None, None),
            "handshake-send"
            | "handshake-receive"
            | "handshake-remote"
            | "handshake-unexpected" => (true, None, false, None, None, None),
            "incompatible" => (true, Some("0.0.1"), false, None, None, None),
            "status-send" | "status-receive" | "status-remote" | "status-unexpected" => {
                (true, Some("0.3.9"), true, None, None, None)
            }
            "complete" => (
                true,
                Some("0.3.9"),
                true,
                Some(3720),
                Some("install"),
                Some("now"),
            ),
            "minutes" => (
                true,
                Some("0.3.9"),
                true,
                Some(75),
                Some("install"),
                Some("now"),
            ),
            "no-operation" => (true, Some("0.3.9"), true, Some(3720), None, Some("now")),
            "no-time" => (true, Some("0.3.9"), true, Some(3720), Some("install"), None),
            _ => panic!("unknown helper fixture: {helper}"),
        };
        serde_json::json!({
            "service_active": service_active,
            "socket_exists": helper != "missing-socket",
            "socket_connectable": connectable,
            "helper_version": version,
            "cli_version": "0.3.2",
            "version_compatible": compatible,
            "uptime_secs": uptime,
            "last_operation": operation,
            "last_operation_time": time,
        })
    }

    #[test]
    fn status_collector_preserves_service_and_helper_evidence() {
        for &(service, state, _) in STATUS_SERVICES {
            for &helper in STATUS_HELPERS {
                let observation = collect_status_fixture(service, helper);
                assert_eq!(observation.service_state, state, "{service}/{helper}");
                assert_eq!(
                    serde_json::to_value(observation.report).unwrap(),
                    expected_status_data(state == StatusServiceState::Active, helper),
                    "{service}/{helper}",
                );
            }
        }
    }

    #[test]
    fn status_preserves_human_and_json_output() {
        let (_, module) = module_path!().split_once("::").unwrap();
        for &(service, state, label) in STATUS_SERVICES {
            // Exhaust evidence combinations in-process; cross output flags on the active fixture.
            let (helpers, modes): (&[&str], &[&str]) = if service == "active" {
                (
                    STATUS_HELPERS,
                    &[
                        "human",
                        "quiet",
                        "local-json",
                        "global-json",
                        "both-json",
                        "quiet-json",
                    ],
                )
            } else {
                (
                    &["complete", "incompatible", "missing-socket"],
                    &["human", "global-json"],
                )
            };
            for &helper in helpers {
                for &mode in modes {
                    let output = std::process::Command::new(std::env::current_exe().unwrap())
                        .args([
                            format!("{module}::status_output_child"),
                            "--exact".to_string(),
                            "--nocapture".to_string(),
                        ])
                        .env("ANOLISA_TEST_SYSTEM_STATUS_SERVICE", service)
                        .env("ANOLISA_TEST_SYSTEM_STATUS_HELPER", helper)
                        .env("ANOLISA_TEST_SYSTEM_STATUS_OUTPUT", mode)
                        .output()
                        .unwrap();
                    assert_eq!(
                        output.status.code(),
                        Some(0),
                        "{service}/{helper}/{mode}: {output:?}"
                    );
                    assert!(output.stderr.is_empty(), "{output:?}");
                    let stdout = String::from_utf8(output.stdout).unwrap();
                    let (_, rendered) = stdout.split_once("STATUS_OUTPUT_BEGIN\n").unwrap();
                    let (rendered, _) = rendered.split_once("STATUS_OUTPUT_END\n").unwrap();
                    assert!(stdout.contains("test result: ok."));
                    if mode.contains("json") {
                        assert_eq!(
                            serde_json::from_str::<serde_json::Value>(rendered).unwrap(),
                            serde_json::json!({
                                "ok": true,
                                "schema_version": response::SCHEMA_VERSION,
                                "command": "system status",
                                "data": expected_status_data(state == StatusServiceState::Active, helper),
                                "warnings": [],
                            }),
                            "{service}/{helper}/{mode}",
                        );
                        continue;
                    }
                    let socket_label = match helper {
                        "missing-socket" => "missing",
                        "connect-failure" => "not connectable",
                        _ => "connected",
                    };
                    let mut expected = format!(
                        "anolisa system helper:\n  Status:      {label}\n  Socket:      {SYSTEM_HELPER_SOCKET} [{socket_label}]\n"
                    );
                    match helper {
                        "incompatible" => expected
                            .push_str("  Version:     0.0.1 (CLI: 0.3.2) ⚠ version mismatch\n"),
                        "status-send" | "status-receive" | "status-remote"
                        | "status-unexpected" | "complete" | "minutes" | "no-operation"
                        | "no-time" => {
                            expected.push_str("  Version:     0.3.9 (CLI: 0.3.2) ✓\n");
                        }
                        _ => {}
                    }
                    match helper {
                        "complete" => expected
                            .push_str("  Uptime:      1h 02m\n  Last op:     install (now)\n"),
                        "minutes" => {
                            expected.push_str("  Uptime:      1m\n  Last op:     install (now)\n")
                        }
                        "no-operation" => expected.push_str("  Uptime:      1h 02m\n"),
                        "no-time" => {
                            expected.push_str("  Uptime:      1h 02m\n  Last op:     install\n")
                        }
                        _ => {}
                    }
                    expected.push('\n');
                    if matches!(service, "missing" | "masked") || helper == "missing-socket" {
                        expected.push_str("  hint: run 'sudo anolisa system setup' to install\n");
                    } else {
                        match helper {
                            "incompatible" => expected.push_str("  warning: CLI and helper versions differ; consider restarting the helper.\n"),
                            // Preserve the existing success hint even when other evidence degrades.
                            "status-send" | "status-receive" | "status-remote" | "status-unexpected"
                            | "complete" | "minutes" | "no-operation" | "no-time" => {
                                expected.push_str("  All checks passed.\n");
                            }
                            _ => {}
                        }
                    }
                    assert_eq!(rendered, expected, "{service}/{helper}/{mode}");
                }
            }
        }
    }

    #[test]
    fn status_output_child() {
        let (_, module) = module_path!().split_once("::").unwrap();
        // An inherited scenario variable must never redirect an ordinary suite invocation.
        if std::env::args().skip(1).collect::<Vec<_>>()
            != [
                format!("{module}::status_output_child"),
                "--exact".to_string(),
                "--nocapture".to_string(),
            ]
        {
            return;
        }
        let service = std::env::var("ANOLISA_TEST_SYSTEM_STATUS_SERVICE").unwrap();
        let helper = std::env::var("ANOLISA_TEST_SYSTEM_STATUS_HELPER").unwrap();
        let mode = std::env::var("ANOLISA_TEST_SYSTEM_STATUS_OUTPUT").unwrap();
        let sandbox = crate::test_support::TestSandbox::new();
        let ctx = sandbox.context_with(
            crate::context::InstallMode::System,
            crate::test_support::TestContextOptions {
                json: matches!(mode.as_str(), "global-json" | "both-json" | "quiet-json"),
                quiet: matches!(mode.as_str(), "quiet" | "quiet-json"),
                ..Default::default()
            },
        );
        let observation = collect_status_fixture(&service, &helper);
        println!("STATUS_OUTPUT_BEGIN");
        render_status(
            matches!(mode.as_str(), "local-json" | "both-json"),
            &ctx,
            observation,
        )
        .unwrap();
        println!("STATUS_OUTPUT_END");
    }

    #[test]
    fn status_capture_environment_does_not_redirect_normal_suite() {
        let (_, module) = module_path!().split_once("::").unwrap();
        for (service, helper, mode) in [
            ("active", "complete", "human"),
            ("invalid", "invalid", "invalid"),
        ] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    format!("{module}::status_"),
                    "--skip".to_string(),
                    format!("{module}::status_capture_environment_does_not_redirect_normal_suite"),
                    "--skip".to_string(),
                    format!("{module}::status_preserves_human_and_json_output"),
                ])
                .env("ANOLISA_TEST_SYSTEM_STATUS_SERVICE", service)
                .env("ANOLISA_TEST_SYSTEM_STATUS_HELPER", helper)
                .env("ANOLISA_TEST_SYSTEM_STATUS_OUTPUT", mode)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            let stdout = String::from_utf8(output.stdout).unwrap();
            assert!(stdout.contains("status_output_child ... ok"), "{stdout}");
            assert!(
                stdout.contains("status_collector_preserves_service_and_helper_evidence ... ok"),
                "{stdout}"
            );
            assert!(stdout.contains("test result: ok."), "{stdout}");
            assert!(
                !stdout.contains("status_preserves_human_and_json_output ..."),
                "{stdout}"
            );
            assert!(!stdout.contains("STATUS_OUTPUT_BEGIN"), "{stdout}");
        }
    }

    const SOCKET_WAIT_SCENARIOS: &[&str] = &[
        "ready",
        "delayed",
        "last-wait",
        "timeout",
        "vanished",
        "final-check",
        "connect",
        "send",
        "receive",
        "incompatible",
        "remote",
        "unexpected",
    ];

    #[derive(Debug, PartialEq, Eq)]
    enum SocketWaitEvent {
        Probe(bool),
        Sleep(Duration),
        Connect,
    }

    fn run_socket_wait_fixture(scenario: &str) -> Result<(), CliError> {
        let (sleeps, loop_probe, final_probe) = match scenario {
            "ready" => (0, true, true),
            "delayed" => (3, true, true),
            "last-wait" => (10, true, true),
            "timeout" => (10, false, false),
            "vanished" => (2, true, false),
            "final-check" => (10, false, true),
            "connect" | "send" | "receive" | "incompatible" | "remote" | "unexpected" => {
                (2, true, true)
            }
            _ => panic!("unknown socket wait fixture: {scenario}"),
        };
        let mut probes: VecDeque<_> = std::iter::repeat_n(false, sleeps)
            .chain([loop_probe, final_probe])
            .collect();
        let mut expected = Vec::new();
        for _ in 0..sleeps {
            expected.push(SocketWaitEvent::Probe(false));
            expected.push(SocketWaitEvent::Sleep(Duration::from_millis(300)));
        }
        expected.extend([
            SocketWaitEvent::Probe(loop_probe),
            SocketWaitEvent::Probe(final_probe),
        ]);
        if final_probe {
            expected.push(SocketWaitEvent::Connect);
        }
        let (sends, receives) = match scenario {
            "timeout" | "vanished" | "connect" => (vec![], vec![]),
            "send" => (
                vec![Err(io::Error::new(io::ErrorKind::BrokenPipe, "send"))],
                vec![],
            ),
            "receive" => (
                vec![],
                vec![Err(io::Error::new(io::ErrorKind::UnexpectedEof, "receive"))],
            ),
            "remote" => (
                vec![],
                vec![Ok(HelperResponse::Error {
                    code: "DENIED".to_string(),
                    message: "no access".to_string(),
                })],
            ),
            "unexpected" => (
                vec![],
                vec![Ok(HelperResponse::Success {
                    message: "wrong response".to_string(),
                    exit_code: 0,
                })],
            ),
            _ => (
                vec![],
                vec![Ok(HelperResponse::HandshakeOk {
                    helper_version: env!("CARGO_PKG_VERSION").to_string(),
                    compatible: scenario != "incompatible",
                })],
            ),
        };
        let (transport, sent) = ScriptedTransport::new(sends, receives);
        let calls = RefCell::new(Vec::new());
        let result = verify_socket_with(
            "system setup",
            || {
                assert!(sent.borrow().is_empty(), "probe after handshake");
                let exists = probes.pop_front().expect("unexpected socket probe");
                calls.borrow_mut().push(SocketWaitEvent::Probe(exists));
                exists
            },
            |duration| {
                assert!(sent.borrow().is_empty(), "sleep after handshake");
                calls.borrow_mut().push(SocketWaitEvent::Sleep(duration));
            },
            || {
                assert!(final_probe, "must not connect after a missing final probe");
                calls.borrow_mut().push(SocketWaitEvent::Connect);
                if scenario == "connect" {
                    return Err(HelperClientError::Connect {
                        path: PathBuf::from("/scripted/system-helper.sock"),
                        source: io::Error::new(io::ErrorKind::ConnectionRefused, "connect"),
                    });
                }
                Ok(HelperClient::with_transport(transport))
            },
        );
        assert!(probes.is_empty(), "{scenario}");
        assert_eq!(*calls.borrow(), expected, "{scenario}");
        let requests = if final_probe && scenario != "connect" {
            vec![HelperRequest::Handshake {
                cli_version: env!("CARGO_PKG_VERSION").to_string(),
            }]
        } else {
            vec![]
        };
        assert_eq!(*sent.borrow(), requests, "{scenario}");
        result
    }

    fn socket_wait_error_reason(scenario: &str) -> Option<String> {
        let reason = match scenario {
            "ready" | "delayed" | "last-wait" | "final-check" => return None,
            "timeout" | "vanished" => {
                return Some(format!(
                    "socket {SYSTEM_HELPER_SOCKET} did not appear within 3 seconds"
                ));
            }
            "connect" => "failed to connect to /scripted/system-helper.sock: connect",
            "send" => "handshake send failed: send",
            "receive" => "handshake recv failed: receive",
            "incompatible" => "handshake succeeded but version is incompatible",
            "remote" => {
                "unexpected handshake response: Error { code: \"DENIED\", message: \"no access\" }"
            }
            "unexpected" => {
                "unexpected handshake response: Success { message: \"wrong response\", exit_code: 0 }"
            }
            _ => panic!("unknown socket wait fixture: {scenario}"),
        };
        Some(reason.to_string())
    }

    #[test]
    fn socket_wait_preserves_probe_sleep_and_handshake_sequence() {
        for &scenario in SOCKET_WAIT_SCENARIOS {
            let result = run_socket_wait_fixture(scenario);
            if let Some(reason) = socket_wait_error_reason(scenario) {
                let error = result.expect_err(scenario);
                assert!(matches!(error, CliError::Runtime { .. }));
                assert_eq!(error.command(), "system setup");
                assert_eq!(error.code(), "EXECUTION_FAILED");
                assert_eq!(error.exit_code(), 1);
                assert_eq!(error.reason(), reason, "{scenario}");
            } else {
                result.expect(scenario);
            }
        }
    }

    #[test]
    fn socket_wait_preserves_output() {
        let (_, module) = module_path!().split_once("::").unwrap();
        for &scenario in SOCKET_WAIT_SCENARIOS {
            for mode in ["human", "json", "quiet"] {
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        format!("{module}::socket_wait_output_child"),
                        "--exact".to_string(),
                        "--nocapture".to_string(),
                    ])
                    .env("ANOLISA_TEST_SOCKET_WAIT_SCENARIO", scenario)
                    .env("ANOLISA_TEST_SOCKET_WAIT_OUTPUT", mode)
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{scenario}/{mode}: {output:?}");
                let stdout = String::from_utf8(output.stdout).unwrap();
                let (_, rendered) = stdout.split_once("SOCKET_WAIT_OUTPUT_BEGIN\n").unwrap();
                let (rendered, _) = rendered.split_once("SOCKET_WAIT_OUTPUT_END\n").unwrap();
                let stderr = String::from_utf8(output.stderr).unwrap();
                if let Some(reason) = socket_wait_error_reason(scenario) {
                    if mode == "json" {
                        assert_eq!(
                            serde_json::from_str::<serde_json::Value>(rendered).unwrap(),
                            serde_json::json!({
                                "ok": false,
                                "schema_version": response::SCHEMA_VERSION,
                                "command": "system setup",
                                "warnings": [],
                                "error": {"code": "EXECUTION_FAILED", "reason": reason},
                            }),
                        );
                        assert!(stderr.is_empty(), "{stderr}");
                    } else {
                        assert!(rendered.is_empty(), "{rendered}");
                        assert_eq!(stderr, format!("error: {reason}\n"));
                    }
                } else {
                    assert!(rendered.is_empty(), "{rendered}");
                    assert_eq!(
                        stderr,
                        "[setup] handshake verified — helper is operational\n"
                    );
                }
                assert!(stdout.contains("test result: ok."), "{stdout}");
            }
        }
    }

    #[test]
    fn socket_wait_output_child() {
        let (_, module) = module_path!().split_once("::").unwrap();
        if std::env::args().skip(1).collect::<Vec<_>>()
            != [
                format!("{module}::socket_wait_output_child"),
                "--exact".to_string(),
                "--nocapture".to_string(),
            ]
        {
            return;
        }
        let scenario = std::env::var("ANOLISA_TEST_SOCKET_WAIT_SCENARIO").unwrap();
        let mode = std::env::var("ANOLISA_TEST_SOCKET_WAIT_OUTPUT").unwrap();
        assert!(["human", "json", "quiet"].contains(&mode.as_str()));
        let sandbox = crate::test_support::TestSandbox::new();
        let ctx = sandbox.context_with(
            crate::context::InstallMode::System,
            crate::test_support::TestContextOptions {
                json: mode == "json",
                quiet: mode == "quiet",
                ..Default::default()
            },
        );
        println!("SOCKET_WAIT_OUTPUT_BEGIN");
        let result = run_socket_wait_fixture(&scenario);
        if socket_wait_error_reason(&scenario).is_some() {
            assert_eq!(
                response::render_error(&ctx, &result.unwrap_err()),
                std::process::ExitCode::from(1),
            );
        } else {
            result.unwrap();
        }
        println!("SOCKET_WAIT_OUTPUT_END");
    }

    #[test]
    fn socket_wait_capture_environment_does_not_redirect_normal_suite() {
        let (_, module) = module_path!().split_once("::").unwrap();
        for (scenario, mode) in [("ready", "human"), ("invalid", "invalid")] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    format!("{module}::socket_wait_"),
                    "--skip".to_string(),
                    format!(
                        "{module}::socket_wait_capture_environment_does_not_redirect_normal_suite"
                    ),
                    "--skip".to_string(),
                    format!("{module}::socket_wait_preserves_output"),
                ])
                .env("ANOLISA_TEST_SOCKET_WAIT_SCENARIO", scenario)
                .env("ANOLISA_TEST_SOCKET_WAIT_OUTPUT", mode)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            let stdout = String::from_utf8(output.stdout).unwrap();
            assert!(
                stdout.contains("socket_wait_output_child ... ok"),
                "{stdout}"
            );
            assert!(
                stdout.contains("socket_wait_preserves_probe_sleep_and_handshake_sequence ... ok"),
                "{stdout}"
            );
            assert!(
                !stdout.contains("socket_wait_preserves_output ..."),
                "{stdout}"
            );
            assert!(!stdout.contains("SOCKET_WAIT_OUTPUT_BEGIN"), "{stdout}");
            assert!(stdout.contains("test result: ok."), "{stdout}");
        }
    }

    #[test]
    fn setup_verification_uses_typed_handshake_result() {
        let compatible = client_with_responses(vec![HelperResponse::HandshakeOk {
            helper_version: env!("CARGO_PKG_VERSION").to_string(),
            compatible: true,
        }]);
        verify_helper_connection("system setup", || Ok(compatible)).expect("compatible helper");

        let incompatible = client_with_responses(vec![HelperResponse::HandshakeOk {
            helper_version: "0.0.1".to_string(),
            compatible: false,
        }]);
        let error = verify_helper_connection("system setup", || Ok(incompatible))
            .expect_err("incompatible helper");
        match error {
            CliError::Runtime { reason, .. } => {
                assert_eq!(reason, "handshake succeeded but version is incompatible");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn setup_verification_preserves_unexpected_response_messages() {
        let cases = [
            (
                HelperResponse::Error {
                    code: "DENIED".to_string(),
                    message: "no access".to_string(),
                },
                "unexpected handshake response: Error { code: \"DENIED\", message: \"no access\" }",
            ),
            (
                HelperResponse::Success {
                    message: "wrong response".to_string(),
                    exit_code: 0,
                },
                "unexpected handshake response: Success { message: \"wrong response\", exit_code: 0 }",
            ),
        ];

        for (response, expected) in cases {
            let client = client_with_responses(vec![response]);
            let error = verify_helper_connection("system setup", || Ok(client))
                .expect_err("unexpected response");

            match error {
                CliError::Runtime { reason, .. } => assert_eq!(reason, expected),
                other => panic!("unexpected error: {other:?}"),
            }
        }
    }

    #[test]
    fn status_marks_connection_failure_as_not_connectable() {
        let result = try_status_connection_with("0.3.2", || Err(connect_error()));

        assert!(!result.connectable);
        assert!(result.handshake.is_none());
        assert!(result.status.is_none());
    }

    #[test]
    fn status_retains_connectability_when_handshake_fails() {
        let (transport, _) = ScriptedTransport::new(
            vec![Err(io::Error::new(io::ErrorKind::BrokenPipe, "send"))],
            Vec::new(),
        );
        let client = HelperClient::with_transport(transport);

        let result = try_status_connection_with("0.3.2", || Ok(client));

        assert!(result.connectable);
        assert!(result.handshake.is_none());
        assert!(result.status.is_none());
    }

    #[test]
    fn status_skips_query_for_incompatible_helper() {
        let client = client_with_responses(vec![HelperResponse::HandshakeOk {
            helper_version: "0.0.1".to_string(),
            compatible: false,
        }]);

        let result = try_status_connection_with("0.3.2", || Ok(client));

        assert!(result.connectable);
        let handshake = result.handshake.expect("handshake evidence");
        assert_eq!(handshake.helper_version, "0.0.1");
        assert!(!handshake.compatible);
        assert!(result.status.is_none());
    }

    #[test]
    fn status_retains_handshake_when_status_query_fails() {
        let client = client_with_responses(vec![
            HelperResponse::HandshakeOk {
                helper_version: "0.3.2".to_string(),
                compatible: true,
            },
            HelperResponse::Error {
                code: "UNAVAILABLE".to_string(),
                message: "status unavailable".to_string(),
            },
        ]);

        let result = try_status_connection_with("0.3.2", || Ok(client));

        assert!(result.connectable);
        assert!(result.handshake.expect("handshake evidence").compatible);
        assert!(result.status.is_none());
    }

    #[test]
    fn status_returns_complete_typed_evidence() {
        let client = client_with_responses(vec![
            HelperResponse::HandshakeOk {
                helper_version: "0.3.2".to_string(),
                compatible: true,
            },
            HelperResponse::Status {
                running: true,
                version: "0.3.2".to_string(),
                uptime_secs: 75,
                last_operation: Some("install".to_string()),
                last_operation_time: Some("now".to_string()),
            },
        ]);

        let result = try_status_connection_with("0.3.2", || Ok(client));

        assert!(result.connectable);
        assert!(result.handshake.expect("handshake evidence").compatible);
        let status = result.status.expect("status evidence");
        assert_eq!(status.uptime_secs, 75);
        assert_eq!(status.last_operation.as_deref(), Some("install"));
        assert_eq!(status.last_operation_time.as_deref(), Some("now"));
    }
}
