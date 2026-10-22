//! Display package plans and replay results without exposing internal recovery records.

use super::{terminal, Options, Outcome};
use aw_package::{database::Scope, plan::Action};
use std::io::{IsTerminal, Write};

pub(super) fn human(outcome: &Outcome, options: &Options) -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    let color = terminal::use_color(options.color, stdout.is_terminal());
    match outcome {
        Outcome::PackagePlanned { plan } => {
            if !options.quiet {
                let action = match plan.action {
                    Action::Install => "Install",
                    Action::Uninstall => "Removal",
                };
                let scope = match plan.scope {
                    Scope::User => "user",
                    Scope::System => "system",
                    Scope::Unknown => "not recorded",
                };
                let title = format!(
                    "{action} plan for {} ({scope})",
                    terminal::escape(&plan.prefix.to_string_lossy())
                );
                writeln!(stdout, "{}", terminal::styled(&title, color, !plan.ready()))?;
                for (action, packages) in [("ADD", &plan.added), ("REMOVE", &plan.removed)] {
                    for package in packages {
                        writeln!(
                            stdout,
                            "{action} {} {}",
                            terminal::escape(&package.package_id),
                            terminal::escape(&package.version)
                        )?;
                    }
                }
                for file in &plan.files {
                    let action = match file.action {
                        Action::Install => "ADD FILE",
                        Action::Uninstall => "REMOVE FILE",
                    };
                    writeln!(
                        stdout,
                        "{action} {:04o} {}",
                        file.mode,
                        terminal::escape(&file.path)
                    )?;
                }
                for dependency in &plan.dependencies {
                    writeln!(
                        stdout,
                        "DEPENDENCY {} -> {} {}",
                        terminal::escape(&dependency.package_id),
                        terminal::escape(&dependency.required_package_id),
                        terminal::escape(&dependency.version)
                    )?;
                }
                if plan.register_legacy {
                    writeln!(stdout, "Older installation records will be archived and registered when this plan is executed.")?;
                }
                writeln!(stdout, "Plan only; no files were created or changed.")?;
            }
            for problem in &plan.problems {
                let path = problem
                    .path
                    .as_ref()
                    .map(|path| format!(" ({})", path.display()))
                    .unwrap_or_default();
                writeln!(
                    stderr,
                    "aw: {}: {}{}",
                    terminal::escape(&problem.code),
                    terminal::escape(&problem.message),
                    terminal::escape(&path)
                )?;
            }
        }
        Outcome::PackageRecovered {
            database_path,
            report,
        } => {
            if !options.quiet {
                writeln!(
                    stdout,
                    "Recovered package operations: {} committed, {} rolled back, {} conflicts",
                    report.committed,
                    report.rolled_back,
                    report.conflicts.len()
                )?;
            }
            for conflict in &report.conflicts {
                writeln!(
                    stderr,
                    "aw: recovery conflict; retained {}",
                    terminal::escape(&conflict.to_string_lossy())
                )?;
            }
            if options.verbose {
                writeln!(
                    stderr,
                    "Database: {}",
                    terminal::escape(&database_path.to_string_lossy())
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}
