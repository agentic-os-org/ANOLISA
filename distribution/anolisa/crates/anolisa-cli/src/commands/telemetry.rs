//! `anolisa telemetry` management surface.
//!
//! Independent command surface for the self-hosted telemetry channel: toggling
//! the collection sentinel, managing the named-reporting link id, showing
//! status, and running the uploader. It deliberately does **not** touch the
//! `register` / `unregister` flow — that stays orthogonal.

use anolisa_core::execution::{CommandOutcomeStatus, ExecutionIntent};
use anolisa_core::{RegistrationManager, TelemetryChannel};
use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::context::CliContext;
use crate::response::{CliError, render_json};

use self::application::{
    TelemetryApplicationOutcome, TelemetryApplied, TelemetryPreview, TelemetryRequest,
};

mod application;

/// systemd unit that runs the resident upload loop.
const SERVICE_NAME: &str = "anolisa-telemetry";
/// Filename of the unit written into the system unit directory.
const UNIT_FILENAME: &str = "anolisa-telemetry.service";
/// User-facing command for inspecting telemetry state.
const STATUS_COMMAND: &str = "anolisa telemetry status";

/// Returns the management command for a telemetry systemd unit target.
pub(crate) fn status_command_for_service_target(target: &str) -> Option<&'static str> {
    matches!(target, SERVICE_NAME | UNIT_FILENAME).then_some(STATUS_COMMAND)
}

#[derive(Parser)]
pub struct TelemetryArgs {
    #[command(subcommand)]
    pub command: TelemetryCommands,
}

#[derive(Subcommand)]
pub enum TelemetryCommands {
    /// Enable default telemetry collection (requires root/sudo)
    Enable,
    /// Disable telemetry collection (requires root/sudo)
    Disable,
    /// Show telemetry collection and link status
    Status {
        /// Output machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Link this instance to named reporting (requires root/sudo)
    Link,
    /// Remove the named-reporting link (requires root/sudo)
    Unlink,
    /// Run the uploader once, or as a loop with `--loop` (internal)
    #[command(hide = true)]
    Upload {
        /// Run the continuous upload loop (daemon mode)
        #[arg(long = "loop")]
        loop_flag: bool,
    },
    /// Self-heal the ops channel without touching consent (internal, boot)
    #[command(hide = true)]
    Init,
}

/// Dispatch `telemetry` subcommands.
pub fn handle(args: TelemetryArgs, ctx: &CliContext) -> Result<(), CliError> {
    match args.command {
        TelemetryCommands::Status { json } => handle_status(json),
        command => handle_mutation(command, ctx),
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct TelemetryPreviewPayload {
    dry_run: bool,
    message: &'static str,
}

fn handle_mutation(command: TelemetryCommands, ctx: &CliContext) -> Result<(), CliError> {
    let request = mutation_request(command, execution_intent(ctx.dry_run));
    render_mutation(application::run(request, ctx)?, ctx)
}

fn mutation_request(command: TelemetryCommands, intent: ExecutionIntent) -> TelemetryRequest {
    match command {
        TelemetryCommands::Enable => TelemetryRequest::Enable { intent },
        TelemetryCommands::Disable => TelemetryRequest::Disable { intent },
        TelemetryCommands::Link => TelemetryRequest::Link { intent },
        TelemetryCommands::Unlink => TelemetryRequest::Unlink { intent },
        TelemetryCommands::Upload { loop_flag } => TelemetryRequest::Upload { loop_flag, intent },
        TelemetryCommands::Init => TelemetryRequest::Init { intent },
        TelemetryCommands::Status { .. } => {
            unreachable!("status is dispatched through the read-only handler")
        }
    }
}

fn execution_intent(dry_run: bool) -> ExecutionIntent {
    if dry_run {
        ExecutionIntent::Plan
    } else {
        ExecutionIntent::Apply
    }
}

/// Enable default collection through the shared telemetry application path.
///
/// The deprecated `register` command uses this apply-only compatibility entry
/// before decommissioning its legacy ilogtail channel.
pub(crate) fn handle_enable(ctx: &CliContext) -> Result<(), CliError> {
    render_mutation(
        application::run(
            TelemetryRequest::Enable {
                intent: ExecutionIntent::Apply,
            },
            ctx,
        )?,
        ctx,
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TelemetryOutput {
    Stdout(String),
    Stderr(String),
}

fn render_mutation(result: TelemetryApplicationOutcome, ctx: &CliContext) -> Result<(), CliError> {
    match result {
        TelemetryApplicationOutcome::Preview(TelemetryPreview { command, message }) => {
            if ctx.json {
                return render_json(
                    command,
                    TelemetryPreviewPayload {
                        dry_run: true,
                        message,
                    },
                );
            }
            if !ctx.quiet {
                println!("[dry-run] {message}");
            }
            Ok(())
        }
        TelemetryApplicationOutcome::Applied { result, outcome } => {
            match outcome.status() {
                CommandOutcomeStatus::Completed => {}
                CommandOutcomeStatus::Partial { reason } => {
                    return Err(CliError::Degraded {
                        command: result.command().to_string(),
                        reason: reason.clone(),
                    });
                }
                CommandOutcomeStatus::Failed { reason } => {
                    return Err(CliError::Runtime {
                        command: result.command().to_string(),
                        reason: reason.clone(),
                    });
                }
            }
            for output in applied_output(&result, outcome.warnings()) {
                match output {
                    TelemetryOutput::Stdout(line) => println!("{line}"),
                    TelemetryOutput::Stderr(line) => eprintln!("{line}"),
                }
            }
            Ok(())
        }
    }
}

fn applied_output(result: &TelemetryApplied, warnings: &[String]) -> Vec<TelemetryOutput> {
    let warning_lines = || {
        warnings
            .iter()
            .map(|warning| TelemetryOutput::Stderr(format!("warn: {warning}")))
            .collect::<Vec<_>>()
    };
    match result {
        TelemetryApplied::Enabled => warning_lines()
            .into_iter()
            .chain([TelemetryOutput::Stdout(
                "Telemetry collection enabled.".to_string(),
            )])
            .collect(),
        TelemetryApplied::Disabled => warning_lines()
            .into_iter()
            .chain([
                TelemetryOutput::Stdout("Telemetry collection disabled.".to_string()),
                TelemetryOutput::Stdout(
                    "  The uploader stops shortly; buffered logs are preserved.".to_string(),
                ),
            ])
            .collect(),
        TelemetryApplied::Linked {
            link_id,
            already_linked: true,
        } => vec![TelemetryOutput::Stdout(format!(
            "Already linked (link id: {link_id})."
        ))],
        TelemetryApplied::Linked {
            link_id,
            already_linked: false,
        } => [
            TelemetryOutput::Stdout("Linked to named reporting.".to_string()),
            TelemetryOutput::Stdout(format!("  link id: {link_id}")),
        ]
        .into_iter()
        .chain(warning_lines())
        .collect(),
        TelemetryApplied::Unlinked => warning_lines()
            .into_iter()
            .chain([TelemetryOutput::Stdout(
                "Unlinked from named reporting.".to_string(),
            )])
            .collect(),
        TelemetryApplied::Uploaded { .. } | TelemetryApplied::Initialized => warning_lines(),
    }
}

// ── status ──────────────────────────────────────────────────────────

struct TelemetryStatusObservation {
    collection_enabled: bool,
    link_id: Option<String>,
}

fn handle_status(json: bool) -> Result<(), CliError> {
    let observation = collect_status_with(
        || TelemetryChannel::new().is_enabled(),
        || RegistrationManager::new().read_link_id(),
    );
    render_status(json, observation)
}

fn collect_status_with<F, L>(collection_enabled: F, read_link_id: L) -> TelemetryStatusObservation
where
    F: FnOnce() -> bool,
    L: FnOnce() -> Option<String>,
{
    let collection_enabled = collection_enabled();
    let link_id = read_link_id();
    TelemetryStatusObservation {
        collection_enabled,
        link_id,
    }
}

fn render_status(json: bool, observation: TelemetryStatusObservation) -> Result<(), CliError> {
    let TelemetryStatusObservation {
        collection_enabled,
        link_id,
    } = observation;
    let linked = link_id.is_some();

    if json {
        return render_json(
            "telemetry status",
            serde_json::json!({
                "collection_enabled": collection_enabled,
                "linked": linked,
                "link_id": link_id,
            }),
        );
    }

    println!(
        "Telemetry collection: {}",
        if collection_enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    match &link_id {
        Some(id) => println!("Named reporting:      linked ({id})"),
        None => println!("Named reporting:      not linked"),
    }
    Ok(())
}

// ── Unit tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use super::*;
    use anolisa_core::telemetry::TelemetryConfig;
    use clap::Parser;

    const STATUS_RECORDS: &[&str] = &[
        "missing",
        "init-linked",
        "registered-linked",
        "unregistered-linked",
        "unlinked",
        "empty-link",
        "v1",
        "corrupt",
        "future-schema",
        "legacy-mode",
        #[cfg(target_os = "linux")]
        "unexpected-mode",
        "read-error",
    ];

    const STATUS_OUTPUT_MODES: &[(&str, &[&str], bool)] = &[
        ("human", &["anolisa", "telemetry", "status"], false),
        (
            "local-json",
            &["anolisa", "telemetry", "status", "--json"],
            true,
        ),
        (
            "global-json",
            &["anolisa", "--json", "telemetry", "status"],
            true,
        ),
        (
            "middle-json",
            &["anolisa", "telemetry", "--json", "status"],
            true,
        ),
        (
            "quiet",
            &["anolisa", "--quiet", "telemetry", "status"],
            false,
        ),
        (
            "quiet-json",
            &["anolisa", "--quiet", "--json", "telemetry", "status"],
            true,
        ),
        (
            "dry-run",
            &["anolisa", "--dry-run", "telemetry", "status"],
            false,
        ),
        (
            "dry-run-json",
            &["anolisa", "--dry-run", "telemetry", "status", "--json"],
            true,
        ),
    ];

    struct StatusFixture {
        tmp: tempfile::TempDir,
        channel: TelemetryChannel,
        registration: RegistrationManager,
        expected_link: Option<&'static str>,
    }

    impl StatusFixture {
        fn new(enabled: bool, record: &str) -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path();
            let marker = root.join(".telemetry_disabled");
            if !enabled {
                fs::write(&marker, "disabled fixture\n").unwrap();
            }
            let config = TelemetryConfig {
                metadata_url: "http://127.0.0.1:9/unused".to_string(),
                ops_dir: root.join("ops"),
                logrotate_config_path: root.join("logrotate"),
                instance_id_cache_path: root.join("instance-id.cache"),
                identity_cache_path: root.join("identity.json"),
                machine_id_path: root.join("machine-id"),
                release_path: root.join("release"),
                os_release_path: root.join("os-release"),
                cpu_present_path: root.join("cpu-present"),
                image_id_path: root.join("image-id"),
                telemetry_id_path: root.join("telemetry-id"),
                legacy_accounts_path: root.join("legacy-accounts.json"),
            };
            let registration = RegistrationManager::with_paths(
                root.join("register.json"),
                config.release_path.clone(),
            );
            let expected_link = match record {
                "init-linked" | "registered-linked" | "unregistered-linked" | "legacy-mode" => {
                    Some("fixture-link")
                }
                "empty-link" => Some(""),
                _ => None,
            };
            match record {
                "missing" => {}
                "read-error" => fs::create_dir(&registration.register_path).unwrap(),
                "corrupt" => fs::write(&registration.register_path, "not valid json {{").unwrap(),
                "v1" => fs::write(
                    &registration.register_path,
                    r#"{"version":1,"state":"registered","registration_time":"2026-01-01T00:00:00Z"}"#,
                ).unwrap(),
                "init-linked" | "registered-linked" | "unregistered-linked" | "unlinked"
                | "empty-link" | "future-schema" | "legacy-mode" | "unexpected-mode" => {
                    let state = match record {
                        "init-linked" => "init",
                        "unregistered-linked" => "unregistered",
                        _ => "registered",
                    };
                    let mut contents = serde_json::json!({
                        "schema_version": if record == "future-schema" { "3" } else { "2" },
                        "state": state,
                        "history": [],
                    });
                    if record != "unlinked" {
                        contents["link_id"] = serde_json::json!(
                            if record == "empty-link" { "" } else { "fixture-link" }
                        );
                    }
                    fs::write(&registration.register_path, contents.to_string()).unwrap();
                }
                _ => panic!("unknown status fixture: {record}"),
            }
            if record != "missing" {
                let mode = match record {
                    "legacy-mode" => 0o600,
                    "unexpected-mode" => 0o640,
                    _ => 0o644,
                };
                // A directory with an accepted mode reaches the read-error path even as root.
                fs::set_permissions(
                    &registration.register_path,
                    fs::Permissions::from_mode(mode),
                )
                .unwrap();
            }
            Self {
                channel: TelemetryChannel::with_paths(config, marker),
                registration,
                tmp,
                expected_link,
            }
        }

        fn collect(&self) -> TelemetryStatusObservation {
            let calls = RefCell::new(Vec::new());
            let observation = collect_status_with(
                || {
                    assert!(calls.borrow().is_empty());
                    calls.borrow_mut().push("collection");
                    self.channel.is_enabled()
                },
                || {
                    assert_eq!(*calls.borrow(), ["collection"]);
                    calls.borrow_mut().push("link");
                    self.registration.read_link_id()
                },
            );
            assert_eq!(*calls.borrow(), ["collection", "link"]);
            assert_eq!(observation.link_id.as_deref(), self.expected_link);
            observation
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct StatusFileEvidence {
        path: PathBuf,
        mode: u32,
        contents: Option<Vec<u8>>,
    }

    fn status_files(root: &Path) -> Vec<StatusFileEvidence> {
        let mut paths = fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        paths.sort();
        paths
            .into_iter()
            .map(|path| {
                let metadata = fs::metadata(&path).unwrap();
                let contents = if metadata.is_file() {
                    Some(fs::read(&path).unwrap())
                } else {
                    // Only the dedicated read-error directory is part of these flat fixtures.
                    assert!(fs::read_dir(&path).unwrap().next().is_none());
                    None
                };
                StatusFileEvidence {
                    path,
                    mode: metadata.permissions().mode(),
                    contents,
                }
            })
            .collect()
    }

    #[test]
    fn status_collects_real_readers_in_order_without_writes() {
        for enabled in [false, true] {
            for &record in STATUS_RECORDS {
                let fixture = StatusFixture::new(enabled, record);
                let before = status_files(fixture.tmp.path());
                let observation = fixture.collect();
                assert_eq!(observation.collection_enabled, enabled, "{record}");
                assert_eq!(status_files(fixture.tmp.path()), before, "{record}");
            }
        }
    }

    #[test]
    fn status_cli_flags_preserve_json_selection() {
        for &(mode, args, expected_json) in STATUS_OUTPUT_MODES {
            let cli = crate::commands::Cli::parse_from(args);
            assert_eq!(cli.json, expected_json, "{mode}");
            assert_eq!(cli.quiet, args.contains(&"--quiet"), "{mode}");
            assert_eq!(cli.dry_run, args.contains(&"--dry-run"), "{mode}");
            let crate::commands::Commands::Management(
                crate::commands::ManagementCommands::Telemetry(TelemetryArgs {
                    command: TelemetryCommands::Status { json },
                }),
            ) = cli.command
            else {
                panic!("expected telemetry status")
            };
            assert_eq!(json, expected_json, "{mode}");
        }
    }

    #[test]
    fn status_preserves_output_and_read_warnings() {
        let (_, module) = module_path!().split_once("::").unwrap();
        for enabled in [false, true] {
            for &record in STATUS_RECORDS {
                // Exercise flag combinations once; degradation needs only human and JSON forms.
                let modes = if record == "registered-linked" {
                    STATUS_OUTPUT_MODES
                } else {
                    &STATUS_OUTPUT_MODES[..2]
                };
                for &(mode, _, json) in modes {
                    let output = std::process::Command::new(std::env::current_exe().unwrap())
                        .args([
                            format!("{module}::status_output_child"),
                            "--exact".to_string(),
                            "--nocapture".to_string(),
                        ])
                        .env("ANOLISA_TEST_TELEMETRY_STATUS_RECORD", record)
                        .env("ANOLISA_TEST_TELEMETRY_STATUS_ENABLED", enabled.to_string())
                        .env("ANOLISA_TEST_TELEMETRY_STATUS_OUTPUT", mode)
                        .output()
                        .unwrap();
                    assert_eq!(
                        output.status.code(),
                        Some(0),
                        "{record}/{enabled}/{mode}: {output:?}"
                    );
                    let stdout = String::from_utf8(output.stdout).unwrap();
                    let path = stdout
                        .lines()
                        .find_map(|line| line.strip_prefix("STATUS_REGISTER_PATH="))
                        .unwrap();
                    let (_, rendered) = stdout.split_once("STATUS_OUTPUT_BEGIN\n").unwrap();
                    let (rendered, _) = rendered.split_once("STATUS_OUTPUT_END\n").unwrap();
                    let link = match record {
                        "init-linked"
                        | "registered-linked"
                        | "unregistered-linked"
                        | "legacy-mode" => Some("fixture-link"),
                        "empty-link" => Some(""),
                        _ => None,
                    };
                    if json {
                        assert_eq!(
                            serde_json::from_str::<serde_json::Value>(rendered).unwrap(),
                            serde_json::json!({
                                "ok": true,
                                "schema_version": crate::response::SCHEMA_VERSION,
                                "command": "telemetry status",
                                "data": {
                                    "collection_enabled": enabled,
                                    "linked": link.is_some(),
                                    "link_id": link,
                                },
                                "warnings": [],
                            })
                        );
                    } else {
                        let collection = if enabled { "enabled" } else { "disabled" };
                        let linked = match link {
                            Some(id) => format!("linked ({id})"),
                            None => "not linked".to_string(),
                        };
                        assert_eq!(
                            rendered,
                            format!(
                                "Telemetry collection: {collection}\nNamed reporting:      {linked}\n"
                            )
                        );
                    }
                    let stderr = String::from_utf8(output.stderr).unwrap();
                    let expected_warning = match record {
                        "corrupt" => {
                            format!("[anolisa] warn: failed to parse {path}; treating as INIT\n")
                        }
                        "future-schema" => format!(
                            "[anolisa] warn: {path} has schema_version 3 (expected <= 2); treating as INIT\n"
                        ),
                        "unexpected-mode" => format!(
                            "[anolisa] warn: {path} has unexpected permissions 640; treating as INIT\n"
                        ),
                        "read-error" => {
                            let reason = stdout
                                .lines()
                                .find_map(|line| line.strip_prefix("STATUS_READ_ERROR="))
                                .unwrap();
                            format!("[anolisa] warn: cannot read {path}: {reason}\n")
                        }
                        _ => String::new(),
                    };
                    assert_eq!(stderr, expected_warning, "{record}/{enabled}/{mode}");
                    assert!(stdout.contains("test result: ok."));
                }
            }
        }
    }

    #[test]
    fn status_output_child() {
        let (_, module) = module_path!().split_once("::").unwrap();
        if std::env::args().skip(1).collect::<Vec<_>>()
            != [
                format!("{module}::status_output_child"),
                "--exact".to_string(),
                "--nocapture".to_string(),
            ]
        {
            return;
        }
        let record = std::env::var("ANOLISA_TEST_TELEMETRY_STATUS_RECORD").unwrap();
        let enabled = std::env::var("ANOLISA_TEST_TELEMETRY_STATUS_ENABLED")
            .unwrap()
            .parse::<bool>()
            .unwrap();
        let mode = std::env::var("ANOLISA_TEST_TELEMETRY_STATUS_OUTPUT").unwrap();
        let (_, args, expected_json) = STATUS_OUTPUT_MODES
            .iter()
            .find(|(name, _, _)| *name == mode)
            .unwrap();
        let cli = crate::commands::Cli::parse_from(*args);
        let crate::commands::Commands::Management(crate::commands::ManagementCommands::Telemetry(
            TelemetryArgs {
                command: TelemetryCommands::Status { json },
            },
        )) = cli.command
        else {
            panic!("expected telemetry status")
        };
        assert_eq!(json, *expected_json);
        let fixture = StatusFixture::new(enabled, &record);
        let before = status_files(fixture.tmp.path());
        println!(
            "STATUS_REGISTER_PATH={}",
            fixture.registration.register_path.display()
        );
        if record == "read-error" {
            println!(
                "STATUS_READ_ERROR={}",
                fs::read_to_string(&fixture.registration.register_path).unwrap_err()
            );
        }
        println!("STATUS_OUTPUT_BEGIN");
        let observation = fixture.collect();
        assert_eq!(observation.collection_enabled, enabled);
        render_status(json, observation).unwrap();
        println!("STATUS_OUTPUT_END");
        assert_eq!(status_files(fixture.tmp.path()), before);
    }

    #[test]
    fn status_capture_environment_does_not_redirect_normal_suite() {
        let (_, module) = module_path!().split_once("::").unwrap();
        for (record, enabled, mode) in [
            ("registered-linked", "true", "human"),
            ("invalid", "invalid", "invalid"),
        ] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    format!("{module}::status_"),
                    "--skip".to_string(),
                    format!("{module}::status_capture_environment_does_not_redirect_normal_suite"),
                    "--skip".to_string(),
                    format!("{module}::status_preserves_output_and_read_warnings"),
                ])
                .env("ANOLISA_TEST_TELEMETRY_STATUS_RECORD", record)
                .env("ANOLISA_TEST_TELEMETRY_STATUS_ENABLED", enabled)
                .env("ANOLISA_TEST_TELEMETRY_STATUS_OUTPUT", mode)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            let stdout = String::from_utf8(output.stdout).unwrap();
            assert!(stdout.contains("status_output_child ... ok"), "{stdout}");
            assert!(
                stdout.contains("status_collects_real_readers_in_order_without_writes ... ok"),
                "{stdout}"
            );
            assert!(
                stdout.contains("status_cli_flags_preserve_json_selection ... ok"),
                "{stdout}"
            );
            assert!(
                !stdout.contains("status_preserves_output_and_read_warnings ..."),
                "{stdout}"
            );
            assert!(!stdout.contains("STATUS_OUTPUT_BEGIN"), "{stdout}");
            assert!(stdout.contains("test result: ok."), "{stdout}");
        }
    }

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: TelemetryCommands,
    }

    fn parse(args: &[&str]) -> TelemetryCommands {
        TestCli::parse_from(args).command
    }

    #[test]
    fn test_parse_enable_disable() {
        assert!(matches!(parse(&["t", "enable"]), TelemetryCommands::Enable));
        assert!(matches!(
            parse(&["t", "disable"]),
            TelemetryCommands::Disable
        ));
    }

    #[test]
    fn test_parse_status_json() {
        assert!(matches!(
            parse(&["t", "status", "--json"]),
            TelemetryCommands::Status { json: true }
        ));
        assert!(matches!(
            parse(&["t", "status"]),
            TelemetryCommands::Status { json: false }
        ));
    }

    #[test]
    fn service_targets_share_the_canonical_status_command() {
        for target in [SERVICE_NAME, UNIT_FILENAME] {
            assert_eq!(
                status_command_for_service_target(target),
                Some("anolisa telemetry status"),
            );
        }
        assert_eq!(status_command_for_service_target("telemetry"), None);
    }

    #[test]
    fn test_parse_link_unlink() {
        assert!(matches!(parse(&["t", "link"]), TelemetryCommands::Link));
        assert!(matches!(parse(&["t", "unlink"]), TelemetryCommands::Unlink));
    }

    #[test]
    fn test_parse_init() {
        assert!(matches!(parse(&["t", "init"]), TelemetryCommands::Init));
    }

    #[test]
    fn test_parse_upload_loop_flag() {
        assert!(matches!(
            parse(&["t", "upload", "--loop"]),
            TelemetryCommands::Upload { loop_flag: true }
        ));
        assert!(matches!(
            parse(&["t", "upload"]),
            TelemetryCommands::Upload { loop_flag: false }
        ));
    }

    #[test]
    fn global_dry_run_maps_to_execution_intent() {
        assert_eq!(execution_intent(true), ExecutionIntent::Plan);
        assert_eq!(execution_intent(false), ExecutionIntent::Apply);
    }

    #[test]
    fn mutation_commands_map_to_typed_requests() {
        assert!(matches!(
            mutation_request(TelemetryCommands::Enable, ExecutionIntent::Plan),
            TelemetryRequest::Enable {
                intent: ExecutionIntent::Plan
            }
        ));
        assert!(matches!(
            mutation_request(
                TelemetryCommands::Upload { loop_flag: true },
                ExecutionIntent::Apply
            ),
            TelemetryRequest::Upload {
                loop_flag: true,
                intent: ExecutionIntent::Apply
            }
        ));
    }

    #[test]
    fn preview_payload_is_machine_readable() {
        let payload = TelemetryPreviewPayload {
            dry_run: true,
            message: "would link this instance to named reporting",
        };
        let value = serde_json::to_value(payload).expect("serialize telemetry preview");

        assert_eq!(value["dry_run"], true);
        assert_eq!(
            value["message"],
            "would link this instance to named reporting"
        );
    }

    #[test]
    fn applied_output_preserves_warning_and_success_order() {
        let warnings = vec!["first warning".to_string(), "second warning".to_string()];
        assert_eq!(
            applied_output(&TelemetryApplied::Enabled, &warnings),
            vec![
                TelemetryOutput::Stderr("warn: first warning".to_string()),
                TelemetryOutput::Stderr("warn: second warning".to_string()),
                TelemetryOutput::Stdout("Telemetry collection enabled.".to_string()),
            ]
        );
        assert_eq!(
            applied_output(
                &TelemetryApplied::Linked {
                    link_id: "link-1".to_string(),
                    already_linked: false,
                },
                &warnings,
            ),
            vec![
                TelemetryOutput::Stdout("Linked to named reporting.".to_string()),
                TelemetryOutput::Stdout("  link id: link-1".to_string()),
                TelemetryOutput::Stderr("warn: first warning".to_string()),
                TelemetryOutput::Stderr("warn: second warning".to_string()),
            ]
        );
        assert_eq!(
            applied_output(&TelemetryApplied::Unlinked, &warnings),
            vec![
                TelemetryOutput::Stderr("warn: first warning".to_string()),
                TelemetryOutput::Stderr("warn: second warning".to_string()),
                TelemetryOutput::Stdout("Unlinked from named reporting.".to_string()),
            ]
        );
    }

    #[test]
    fn already_linked_output_keeps_existing_message() {
        assert_eq!(
            applied_output(
                &TelemetryApplied::Linked {
                    link_id: "existing".to_string(),
                    already_linked: true,
                },
                &[],
            ),
            vec![TelemetryOutput::Stdout(
                "Already linked (link id: existing).".to_string()
            )]
        );
    }

    #[test]
    fn test_unit_template_renders_exec_and_wantedby() {
        const UNIT_TEMPLATE: &str =
            include_str!("../../../../systemd/anolisa-telemetry.service.in");
        let rendered = UNIT_TEMPLATE.replace("@@ANOLISA_BIN@@", "/usr/bin/anolisa");
        assert!(rendered.contains("ExecStartPre=/usr/bin/anolisa telemetry init"));
        assert!(rendered.contains("ExecStart=/usr/bin/anolisa telemetry upload --loop"));
        assert!(rendered.contains("WantedBy=multi-user.target"));
        // Placeholder must be fully substituted.
        assert!(!rendered.contains("@@ANOLISA_BIN@@"));
    }
}
