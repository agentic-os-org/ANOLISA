//! Versioned management reports leave native streams and legacy JSON untouched.

use serde::Serialize;
use std::{
    io::{IsTerminal, Write},
    path::PathBuf,
};

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
    human_with(
        outcome,
        &Options {
            color: Some(Color::Never),
            ..Options::default()
        },
    )
}

fn human_with(outcome: &Outcome, options: &Options) -> std::io::Result<()> {
    if options.quiet {
        return Ok(());
    }
    let mut stdout = std::io::stdout().lock();
    let summary = match outcome {
        Outcome::Installed { prefix } => format!(
            "Installed verified AW components in {}",
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
            "Removed AW package files; external configuration and state retained".into()
        }
        Outcome::ConfigurationEdited { changed: true } => "Updated configuration".into(),
        Outcome::ConfigurationEdited { changed: false } => "Configuration unchanged".into(),
        Outcome::ConfigurationValidated => "Valid configuration".into(),
    };
    let colored = terminal::use_color(options.color, stdout.is_terminal());
    writeln!(stdout, "{}", terminal::styled(&summary, colored, false))?;
    if options.verbose {
        let detail = match outcome {
            Outcome::Installed { .. } => {
                "Package files and receipts were published; no runtime activation was requested."
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
            Outcome::ConfigurationValidated => "Configuration validation completed offline.",
        };
        writeln!(std::io::stderr().lock(), "{detail}")?;
    }
    Ok(())
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
    options: Options,
    result: aw_package::Result<Outcome>,
) -> crate::cli::Result<crate::cli::Exit> {
    match result {
        Ok(outcome) => {
            if options.json {
                json(command, ResultView::Succeeded { result: &outcome })?;
            } else {
                human_with(&outcome, &options)?;
            }
            Ok(crate::cli::Exit::Code(0))
        }
        Err(error) if options.json => {
            // Failure describes the command, not rollback or runtime health.
            json(
                command,
                ResultView::Failed {
                    error: Diagnostic::from(&error),
                },
            )?;
            Ok(crate::cli::Exit::Code(1))
        }
        Err(error) => {
            let mut stderr = std::io::stderr().lock();
            let message = format!("aw: {}", terminal::escape(&error.to_string()));
            let colored = terminal::use_color(options.color, stderr.is_terminal());
            writeln!(stderr, "{}", terminal::styled(&message, colored, true))?;
            Ok(crate::cli::Exit::Code(1))
        }
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
