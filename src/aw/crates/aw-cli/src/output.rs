//! Versioned management reports leave native streams and legacy JSON untouched.

use serde::Serialize;
use std::{
    io::{IsTerminal, Write},
    path::PathBuf,
};

mod plans;
mod queries;
mod terminal;

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq)]
pub(crate) enum Color {
    Auto,
    Always,
    Never,
}

#[derive(clap::Args, Debug, Default, PartialEq)]
pub(crate) struct Options {
    /// Emit a versioned management report instead of the human summary
    #[arg(long, conflicts_with_all = ["quiet", "verbose", "color"])]
    pub json: bool,
    /// Suppress the human success summary; keep errors and cleanup warnings
    #[arg(long, conflicts_with = "verbose")]
    pub quiet: bool,
    /// Add fixed operation-scope details on stderr, without private values
    #[arg(long)]
    pub verbose: bool,
    /// Management summary colors (default: auto); never changes protocol streams
    #[arg(long, value_enum)]
    pub color: Option<Color>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Outcome {
    Installed {
        prefix: PathBuf,
    },
    Configured {
        config: PathBuf,
        provider_template: String,
    },
    Uninstalled,
    ConfigurationEdited {
        changed: bool,
    },
    ConfigurationChecked {
        policy_update_scope: bool,
    },
    PackagesListed {
        inventory: aw_package::database::Inventory,
    },
    PackageFilesChecked {
        report: aw_package::database::DoctorReport,
    },
    PackagePlanned {
        plan: Box<aw_package::plan::PackagePlan>,
    },
    PackageRecovered {
        database_path: PathBuf,
        report: aw_package::transaction::RecoveryReport,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum ErrorCode {
    Io,
    InvalidJson,
    InvalidYaml,
    PackageContract,
    InvalidConfiguration,
    Cancelled,
    ConfirmationRequired,
    ConfirmationDeclined,
    NeedsRecovery,
    IntegrityCheck,
    PlanBlocked,
    PackageInUse,
}

#[derive(Serialize)]
struct Diagnostic {
    code: ErrorCode,
    message: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery_path: Option<PathBuf>,
}

impl From<&aw_package::Error> for Diagnostic {
    fn from(error: &aw_package::Error) -> Self {
        use aw_package::Error;
        // Library details can contain a schema value or private configuration.
        // This deliberately coarse public mapping does not inspect error text.
        let (code, message) = match error {
            Error::InUse(_) => (
                ErrorCode::PackageInUse,
                "The installation is still in use by AW or another package operation",
            ),
            Error::Io(_) | Error::Filesystem { .. } => {
                (ErrorCode::Io, "A filesystem or process operation failed")
            }
            Error::Json(_) => (ErrorCode::InvalidJson, "Package JSON processing failed"),
            Error::Yaml(_) => (
                ErrorCode::InvalidYaml,
                "Configuration YAML processing failed",
            ),
            Error::Invalid(_) => (
                ErrorCode::PackageContract,
                "The operation violates the package or configuration contract",
            ),
            Error::Cancelled => (ErrorCode::Cancelled, "The package operation was cancelled"),
            Error::NeedsRecovery(_) => (
                ErrorCode::NeedsRecovery,
                "A package operation needs recovery before another package operation can continue",
            ),
        };
        Self {
            code,
            message,
            recovery_path: match error {
                Error::NeedsRecovery(path) => Some(path.clone()),
                _ => None,
            },
        }
    }
}

#[derive(Serialize)]
struct Report<'a> {
    format: u8,
    command: &'static str,
    #[serde(flatten)]
    result: ResultView<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mutation: Option<mutation::Summary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<mutation::Warning>,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ResultView<'a> {
    Succeeded {
        result: &'a Outcome,
    },
    Failed {
        error: Diagnostic,
    },
    Cancelled {
        error: Diagnostic,
    },
    Partial {
        error: Diagnostic,
    },
    #[serde(rename = "failed")]
    CheckFailed {
        error: Diagnostic,
        result: &'a Outcome,
    },
    #[serde(rename = "partial")]
    RecoveryPartial {
        error: Diagnostic,
        result: &'a Outcome,
    },
}

mod mutation;

pub(crate) enum ValidationError {
    Io(std::io::Error),
    Configuration(aw_config::Error),
}

struct Failure {
    human: String,
    diagnostic: Diagnostic,
}

pub(crate) enum NotConfirmed {
    Required,
    Declined,
    Interrupted(i32),
}

pub(crate) fn removal_prompt(prefix: &str) -> std::io::Result<()> {
    let mut stderr = std::io::stderr().lock();
    write!(
        stderr,
        "Remove package-owned files from {}? [y/N] ",
        terminal::escape(prefix)
    )?;
    stderr.flush()
}

pub(crate) fn unconfirmed(
    options: Options,
    reason: NotConfirmed,
) -> crate::cli::Result<crate::cli::Exit> {
    let (code, message, exit) = match reason {
        NotConfirmed::Required => (ErrorCode::ConfirmationRequired,
            "Uninstall requires --yes for JSON or non-interactive input; no package operation was started", 1),
        NotConfirmed::Declined => (ErrorCode::ConfirmationDeclined,
            "Removal declined; no package operation was started", 1),
        NotConfirmed::Interrupted(signal) => (ErrorCode::Cancelled,
            "Removal confirmation interrupted; no package operation was started", 128 + signal),
    };
    complete_result(
        "uninstall",
        options,
        Err(Failure {
            human: message.into(),
            diagnostic: Diagnostic {
                code,
                message,
                recovery_path: None,
            },
        }),
        None,
    )?;
    Ok(crate::cli::Exit::Code(exit))
}

fn human_with(outcome: &Outcome, options: &Options) -> std::io::Result<()> {
    if matches!(
        outcome,
        Outcome::PackagePlanned { .. } | Outcome::PackageRecovered { .. }
    ) {
        return plans::human(outcome, options);
    }
    if matches!(
        outcome,
        Outcome::PackagesListed { .. } | Outcome::PackageFilesChecked { .. }
    ) {
        return queries::human(outcome, options);
    }
    if options.quiet {
        return Ok(());
    }
    let mut stdout = std::io::stdout().lock();
    let summary = match outcome {
        Outcome::Installed { prefix } => format!(
            "Installed verified packages in {}",
            terminal::escape(&prefix.to_string_lossy())
        ),
        Outcome::Configured {
            config,
            provider_template,
        } => format!(
            "Created {}; Provider template: {}",
            terminal::escape(&config.to_string_lossy()),
            terminal::escape(provider_template)
        ),
        Outcome::Uninstalled => {
            "Removed package files; external configuration and state retained".into()
        }
        Outcome::ConfigurationEdited { changed: true } => "Updated configuration".into(),
        Outcome::ConfigurationEdited { changed: false } => "Configuration unchanged".into(),
        Outcome::ConfigurationChecked {
            policy_update_scope: false,
        } => "configuration valid".into(),
        Outcome::ConfigurationChecked {
            policy_update_scope: true,
        } => "policy update scope valid; runtime activation is not checked".into(),
        Outcome::PackagesListed { .. } | Outcome::PackageFilesChecked { .. } => {
            return queries::human(outcome, options);
        }
        Outcome::PackagePlanned { .. } | Outcome::PackageRecovered { .. } => {
            return plans::human(outcome, options)
        }
    };
    let colored = terminal::use_color(options.color, stdout.is_terminal());
    writeln!(stdout, "{}", terminal::styled(&summary, colored, false))?;
    if options.verbose {
        let detail = match outcome {
            Outcome::Installed { .. } => {
                "Package files and database records were published; no runtime activation was requested."
            }
            Outcome::Configured { .. } => {
                "Configuration was created offline; no service reload was requested."
            }
            Outcome::Uninstalled => {
                "Owned package files were removed; external configuration and state were retained."
            }
            Outcome::ConfigurationEdited { changed: true } => {
                "A validated configuration was published; no runtime reload was requested."
            }
            Outcome::ConfigurationEdited { changed: false } => {
                "Configuration bytes and inode were retained; no runtime reload was requested."
            }
            Outcome::ConfigurationChecked { .. } => {
                "Configuration validation completed offline; no runtime activation was checked."
            }
            Outcome::PackagesListed { .. } | Outcome::PackageFilesChecked { .. } => {
                "Installed-package records were read without changing files."
            }
            Outcome::PackagePlanned { .. } | Outcome::PackageRecovered { .. } => {
                unreachable!("package plans and recovery use their own presentation above")
            }
        };
        writeln!(std::io::stderr().lock(), "{detail}")?;
    }
    Ok(())
}

fn json(
    command: &'static str,
    result: ResultView<'_>,
    mutation: Option<&aw_package::MutationReport>,
) -> crate::cli::Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(
        &mut stdout,
        &Report {
            format: 1,
            command,
            result,
            mutation: mutation.map(mutation::Summary::from),
            warnings: mutation.map(mutation::warnings).unwrap_or_default(),
        },
    )?;
    writeln!(stdout)?;
    Ok(())
}

pub(crate) fn complete(
    command: &'static str,
    options: Options,
    result: aw_package::Result<Outcome>,
) -> crate::cli::Result<crate::cli::Exit> {
    complete_result(
        command,
        options,
        result.map_err(|error| Failure {
            diagnostic: Diagnostic::from(&error),
            human: error.to_string(),
        }),
        None,
    )
}

pub(crate) fn complete_validation(
    options: Options,
    result: Result<Outcome, ValidationError>,
) -> crate::cli::Result<crate::cli::Exit> {
    let result = result.map_err(|error| {
        let (code, message, human) = match error {
            ValidationError::Io(error) => (
                ErrorCode::Io,
                "A configuration file could not be read",
                error.to_string(),
            ),
            ValidationError::Configuration(error) => (
                ErrorCode::InvalidConfiguration,
                "Configuration validation failed",
                error.to_string(),
            ),
        };
        Failure {
            human,
            diagnostic: Diagnostic {
                code,
                message,
                recovery_path: None,
            },
        }
    });
    complete_result("config validate", options, result, None)
}

pub(crate) fn complete_mutation(
    command: &'static str,
    options: Options,
    outcome: Outcome,
    result: aw_package::MutationResult,
) -> crate::cli::Result<crate::cli::Exit> {
    match result {
        Ok(report) => complete_result(command, options, Ok(outcome), Some(&report)),
        Err(failure) => complete_result(
            command,
            options,
            Err(Failure {
                diagnostic: Diagnostic::from(&failure.error),
                human: failure.error.to_string(),
            }),
            Some(&failure.report),
        ),
    }
}

fn complete_result(
    command: &'static str,
    options: Options,
    result: Result<Outcome, Failure>,
    mutation: Option<&aw_package::MutationReport>,
) -> crate::cli::Result<crate::cli::Exit> {
    if !options.json {
        if let Some(report) = mutation {
            mutation::human(report)?;
        }
    }
    match result {
        Ok(outcome) => {
            let problem = match &outcome {
                Outcome::PackageFilesChecked { report } if !report.healthy() => Some(Diagnostic {
                    code: ErrorCode::IntegrityCheck,
                    message: "Installed software checks found problems or could not be completed",
                    recovery_path: None,
                }),
                Outcome::PackagePlanned { plan } if !plan.ready() => Some(Diagnostic {
                    code: ErrorCode::PlanBlocked,
                    message: "Package changes are blocked; inspect the plan problems",
                    recovery_path: None,
                }),
                Outcome::PackageRecovered {
                    database_path,
                    report,
                } if !report.conflicts.is_empty() => Some(Diagnostic {
                    code: ErrorCode::NeedsRecovery,
                    message: "Recovery retained conflicting files for explicit inspection",
                    recovery_path: database_path.parent().map(|path| path.join("transactions")),
                }),
                _ => None,
            };
            let unhealthy = problem.is_some();
            if options.json {
                let result = if let Some(error) = problem {
                    if matches!(outcome, Outcome::PackageRecovered { .. }) {
                        ResultView::RecoveryPartial {
                            error,
                            result: &outcome,
                        }
                    } else {
                        ResultView::CheckFailed {
                            error,
                            result: &outcome,
                        }
                    }
                } else {
                    ResultView::Succeeded { result: &outcome }
                };
                json(command, result, mutation)?;
            } else {
                human_with(&outcome, &options)?;
            }
            Ok(crate::cli::Exit::Code(i32::from(unhealthy)))
        }
        Err(failure) if options.json => {
            // Failure describes the command, not rollback or runtime health.
            let result = failure_view(failure.diagnostic, mutation);
            json(command, result, mutation)?;
            Ok(crate::cli::Exit::Code(1))
        }
        Err(failure) => {
            let mut stderr = std::io::stderr().lock();
            let message = format!("aw: {}", terminal::escape(&failure.human));
            let colored = terminal::use_color(options.color, stderr.is_terminal());
            writeln!(stderr, "{}", terminal::styled(&message, colored, true))?;
            Ok(crate::cli::Exit::Code(1))
        }
    }
}

fn failure_view(
    diagnostic: Diagnostic,
    mutation: Option<&aw_package::MutationReport>,
) -> ResultView<'static> {
    if mutation.is_some_and(|report| report.recovery == aw_package::RecoveryState::Incomplete) {
        ResultView::Partial { error: diagnostic }
    } else if matches!(
        diagnostic.code,
        ErrorCode::Cancelled | ErrorCode::ConfirmationDeclined
    ) {
        ResultView::Cancelled { error: diagnostic }
    } else {
        ResultView::Failed { error: diagnostic }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_do_not_reflect_raw_errors_or_claim_rollback() {
        for error in [
            aw_package::Error::Invalid("private-api-key\u{1b}[2J".into()),
            aw_package::Error::Io(std::io::Error::other("private-api-key")),
            aw_package::Error::Filesystem {
                operation: "openat(O_TMPFILE)",
                path: PathBuf::from("/private-api-key"),
                source: std::io::Error::other("private-api-key"),
            },
            aw_package::Error::Json(serde_json::from_str::<u8>("\"private-api-key\"").unwrap_err()),
            aw_package::Error::Yaml(serde_yaml_ng::from_str::<u8>("private-api-key").unwrap_err()),
        ] {
            let value = serde_json::to_value(Report {
                format: 1,
                command: "config add-provider",
                result: ResultView::Failed {
                    error: Diagnostic::from(&error),
                },
                mutation: None,
                warnings: Vec::new(),
            })
            .unwrap();
            assert_eq!(value["format"], 1);
            assert_eq!(value["status"], "failed");
            assert!(value.get("result").is_none());
            for absent in ["private-api-key", "rollback", "changes", "runtime"] {
                assert!(!value.to_string().contains(absent));
            }
        }
    }

    #[test]
    fn cancellation_and_incomplete_recovery_keep_the_primary_cause() {
        for (recovery, status) in [
            (aw_package::RecoveryState::NotAttempted, "cancelled"),
            (aw_package::RecoveryState::Complete, "cancelled"),
            (aw_package::RecoveryState::Incomplete, "partial"),
        ] {
            let report = aw_package::MutationReport {
                committed: false,
                recovery,
                notices: vec![aw_package::MutationNotice {
                    code: aw_package::NoticeCode::BackupRetained,
                    path: "/owned/backup\n\u{1b}[2J".into(),
                    io_kind: None,
                }],
            };
            let value = serde_json::to_value(Report {
                format: 1,
                command: "uninstall",
                result: failure_view(
                    Diagnostic::from(&aw_package::Error::Cancelled),
                    Some(&report),
                ),
                mutation: Some(mutation::Summary::from(&report)),
                warnings: mutation::warnings(&report),
            })
            .unwrap();
            assert_eq!(value["status"], status);
            assert_eq!(value["error"]["code"], "cancelled");
            assert_eq!(value["mutation"]["committed"], false);
            assert_eq!(value["warnings"][0]["code"], "backup_retained");
            assert!(value.get("result").is_none());
        }
    }

    #[test]
    fn committed_cleanup_warning_does_not_claim_operation_failure() {
        let report = aw_package::MutationReport {
            committed: true,
            recovery: aw_package::RecoveryState::NotAttempted,
            notices: vec![aw_package::MutationNotice {
                code: aw_package::NoticeCode::BackupCleanupFailed,
                path: "/owned/backup".into(),
                io_kind: Some(std::io::ErrorKind::PermissionDenied),
            }],
        };
        let value = serde_json::to_value(Report {
            format: 1,
            command: "uninstall",
            result: ResultView::Succeeded {
                result: &Outcome::Uninstalled,
            },
            mutation: Some(mutation::Summary::from(&report)),
            warnings: mutation::warnings(&report),
        })
        .unwrap();
        assert_eq!(value["status"], "succeeded");
        assert_eq!(value["mutation"]["committed"], true);
        assert_eq!(value["mutation"]["recovery"], "not_attempted");
        assert_eq!(value["warnings"][0]["io_kind"], "permission_denied");
        assert!(value.get("error").is_none());
    }
}
