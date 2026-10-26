//! Keep recorded installation details distinct from observed file problems.

use super::{terminal, Options, Outcome};
use aw_package::database::{InventorySource, Manager, Registration, Scope};
use std::io::{IsTerminal, Write};

pub(super) fn human(outcome: &Outcome, options: &Options) -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    let stdout_color = terminal::use_color(options.color, stdout.is_terminal());
    let stderr_color = terminal::use_color(options.color, stderr.is_terminal());
    match outcome {
        Outcome::PackagesListed { inventory } => {
            if !options.quiet {
                if inventory.installations.is_empty() {
                    let summary = format!(
                        "No installations recorded in {}",
                        terminal::escape(&inventory.database_path.to_string_lossy())
                    );
                    writeln!(
                        stdout,
                        "{}",
                        terminal::styled(&summary, stdout_color, false)
                    )?;
                } else {
                    writeln!(
                        stdout,
                        "{}",
                        terminal::styled(
                            "PACKAGE  VERSION  DIRECTORY  SCOPE  MANAGER  RECORDS",
                            stdout_color,
                            false
                        )
                    )?;
                    for installation in &inventory.installations {
                        for package in &installation.packages {
                            writeln!(
                                stdout,
                                "{}  {}  {}  {}  {}  {}",
                                terminal::escape(&package.package_id),
                                terminal::escape(&package.version),
                                terminal::escape(&installation.prefix.to_string_lossy()),
                                match installation.scope {
                                    Scope::User => "user",
                                    Scope::System => "system",
                                    Scope::Unknown => "not recorded",
                                },
                                match installation.manager {
                                    Manager::Raw => "AW",
                                    Manager::Rpm => "RPM",
                                    Manager::Deb => "dpkg",
                                },
                                match installation.registration {
                                    Registration::Registered => "package database",
                                    Registration::LegacyUnregistered =>
                                        "older installation records",
                                }
                            )?;
                        }
                    }
                }
            }
            if options.verbose {
                writeln!(
                    stderr,
                    "Database: {}; records: {}. File contents and runtime health were not checked.",
                    terminal::escape(&inventory.database_path.to_string_lossy()),
                    source(inventory.source)
                )?;
            }
        }
        Outcome::PackageFilesChecked { report } => {
            if !options.quiet {
                let summary = if report.packages_checked == 0 {
                    "No recorded software packages to check".to_owned()
                } else {
                    format!(
                        "Checked {} packages and {} files; {} problems found",
                        report.packages_checked,
                        report.files_checked,
                        report.issues.len()
                    )
                };
                writeln!(
                    stdout,
                    "{}",
                    terminal::styled(&summary, stdout_color, !report.healthy())
                )?;
            }
            for issue in &report.issues {
                let detail = format!(
                    "aw: {:?}: {}: {}",
                    issue.code,
                    issue.path.display(),
                    issue.message
                );
                writeln!(
                    stderr,
                    "{}",
                    terminal::styled(&terminal::escape(&detail), stderr_color, true)
                )?;
            }
            if options.verbose {
                writeln!(
                    stderr,
                    "Database: {}; records: {}. No files were repaired and no Provider was executed.",
                    terminal::escape(&report.database_path.to_string_lossy()),
                    source(report.source)
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn source(value: InventorySource) -> &'static str {
    match value {
        InventorySource::Absent => "no package database",
        InventorySource::Database => "package database",
        InventorySource::Legacy => "older installation records",
    }
}
