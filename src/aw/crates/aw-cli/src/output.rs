//! Versioned management reports leave native streams and legacy JSON untouched.

use serde::Serialize;
use std::{io::Write, path::PathBuf};

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
    ConfigurationValidated,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum ErrorCode {
    Io,
    InvalidJson,
    InvalidYaml,
    PackageContract,
}

#[derive(Serialize)]
struct Diagnostic {
    code: ErrorCode,
    message: &'static str,
}

impl From<&aw_package::Error> for Diagnostic {
    fn from(error: &aw_package::Error) -> Self {
        use aw_package::Error;
        // Library details can contain a schema value or private configuration.
        // This deliberately coarse public mapping does not inspect error text.
        let (code, message) = match error {
            Error::Io(_) => (ErrorCode::Io, "A filesystem or process operation failed"),
            Error::Json(_) => (ErrorCode::InvalidJson, "Package JSON processing failed"),
            Error::Yaml(_) => (
                ErrorCode::InvalidYaml,
                "Configuration YAML processing failed",
            ),
            Error::Invalid(_) => (
                ErrorCode::PackageContract,
                "The operation violates the package or configuration contract",
            ),
        };
        Self { code, message }
    }
}

#[derive(Serialize)]
struct Report<'a> {
    format: u8,
    command: &'static str,
    #[serde(flatten)]
    result: ResultView<'a>,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ResultView<'a> {
    Succeeded { result: &'a Outcome },
    Failed { error: Diagnostic },
}

pub(crate) fn human(outcome: &Outcome) -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    match outcome {
        Outcome::Installed { prefix } => writeln!(
            stdout,
            "Installed verified AW components in {}",
            prefix.display()
        ),
        Outcome::Configured {
            config,
            provider_template,
        } => writeln!(
            stdout,
            "Created {}; Provider template: {provider_template}",
            config.display()
        ),
        Outcome::Uninstalled => writeln!(
            stdout,
            "Removed AW package files; external configuration and state retained"
        ),
        Outcome::ConfigurationEdited { changed: true } => writeln!(stdout, "Updated configuration"),
        Outcome::ConfigurationEdited { changed: false } => {
            writeln!(stdout, "Configuration unchanged")
        }
        Outcome::ConfigurationValidated => writeln!(stdout, "Valid configuration"),
    }
}

fn json(command: &'static str, result: ResultView<'_>) -> crate::cli::Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(
        &mut stdout,
        &Report {
            format: 1,
            command,
            result,
        },
    )?;
    writeln!(stdout)?;
    Ok(())
}

pub(crate) fn complete(
    command: &'static str,
    as_json: bool,
    result: aw_package::Result<Outcome>,
) -> crate::cli::Result<crate::cli::Exit> {
    match result {
        Ok(outcome) => {
            if as_json {
                json(command, ResultView::Succeeded { result: &outcome })?;
            } else {
                human(&outcome)?;
            }
            Ok(crate::cli::Exit::Code(0))
        }
        Err(error) if as_json => {
            // Failure describes the command, not rollback or runtime health.
            json(
                command,
                ResultView::Failed {
                    error: Diagnostic::from(&error),
                },
            )?;
            Ok(crate::cli::Exit::Code(1))
        }
        Err(error) => Err(error.into()),
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
            aw_package::Error::Json(serde_json::from_str::<u8>("\"private-api-key\"").unwrap_err()),
            aw_package::Error::Yaml(serde_yaml_ng::from_str::<u8>("private-api-key").unwrap_err()),
        ] {
            let value = serde_json::to_value(Report {
                format: 1,
                command: "config add-provider",
                result: ResultView::Failed {
                    error: Diagnostic::from(&error),
                },
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
}
