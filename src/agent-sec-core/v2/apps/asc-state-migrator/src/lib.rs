//! Library entry of `asc-state-migrator`, kept testable without a process.

pub mod cli;
pub mod discovery;
pub mod error;
pub mod import;
pub mod journal;
pub(crate) mod observability;
pub mod report;
pub mod source;

use std::path::Path;

use crate::cli::{Cli, Command, CommonArgs};
use crate::discovery::{DiscoveryOptions, OwnerMap};
use crate::error::MigratorError;
use crate::import::ApplyOptions;

/// Runs one parsed command line.
///
/// # Errors
///
/// Returns the first failure; the binary maps it to exit code 1 and prints
/// `error: <message>` on stderr.
pub fn run(cli: &Cli) -> Result<(), MigratorError> {
    match &cli.command {
        Command::Plan => {
            let destination = import::resolve_destination(cli.common.destination.as_deref())?;
            let options = discovery_options(&cli.common, &destination)?;
            let report = import::plan(
                &options,
                &destination,
                retention_days(&cli.common),
                cli.common.force,
                cli.common.writer_grace,
                // A `--sqlite-only` plan reports the same stream selection
                // the apply would use.
                !cli.common.sqlite_only,
            )?;
            emit(&report, &report::render_plan(&report), cli.common.json);
            Ok(())
        }
        Command::Apply => {
            let destination = import::resolve_destination(cli.common.destination.as_deref())?;
            let options = discovery_options(&cli.common, &destination)?;
            let (scans, rejected, system_owned) = import::scan_sources(
                &options,
                cli.common.force,
                cli.common.writer_grace,
                asc_sqlite_kernel::current_epoch(),
                !cli.common.sqlite_only,
            );

            // An explicitly requested source that failed validation is a hard
            // error; scan-found rejections stay informational.
            for rejection in &rejected {
                if is_explicit(&rejection.dir, &cli.common.sources) {
                    return Err(MigratorError::SourceUnusable {
                        path: rejection.dir.display().to_string(),
                        reason: rejection.reason.clone(),
                    });
                }
            }
            if scans.is_empty() {
                return Err(MigratorError::NoSources);
            }

            let apply_options = ApplyOptions {
                retention_days: retention_days(&cli.common),
                jsonl_recovery: !cli.common.sqlite_only,
                observability_retention_days: observability_retention_days(&cli.common),
                // `--sqlite-only` means no JSONL stream of either kind: the
                // scan already skipped the observability log.
                observability_jsonl_recovery: cli.common.recover_observability_jsonl
                    && !cli.common.sqlite_only,
                now_epoch: asc_sqlite_kernel::current_epoch(),
            };
            let report = import::apply(&scans, &rejected, &destination, &apply_options)?;
            emit(&report, &report::render_apply(&report), cli.common.json);
            let _ = system_owned;
            Ok(())
        }
        Command::Verify { run_id } => {
            let destination = import::resolve_destination(cli.common.destination.as_deref())?;
            let report = import::verify(&destination, run_id.as_deref())?;
            // Both destinations must be intact: a corruption diagnostic in
            // either store fails the command even when every journaled run
            // still matches.
            let quick_check_ok =
                report.quick_check == "ok" && report.observability_quick_check == "ok";
            let runs_ok = report.runs.iter().all(|run| run.ok);
            emit(&report, &report::render_verify(&report), cli.common.json);
            if !quick_check_ok || !runs_ok {
                return Err(MigratorError::Usage(if quick_check_ok {
                    "verification found mismatches".to_owned()
                } else {
                    "verification failed the destination integrity check".to_owned()
                }));
            }
            Ok(())
        }
        Command::Rollback { run_id, all } => {
            let destination = import::resolve_destination(cli.common.destination.as_deref())?;
            let report = import::rollback(&destination, run_id.as_deref(), *all)?;
            emit(&report, &report::render_rollback(&report), cli.common.json);
            Ok(())
        }
    }
}

fn discovery_options(
    common: &CommonArgs,
    destination: &Path,
) -> Result<DiscoveryOptions, MigratorError> {
    let owner_map = OwnerMap::parse(&common.map_owner).map_err(MigratorError::Usage)?;
    Ok(DiscoveryOptions {
        explicit: common.sources.clone(),
        owner_map,
        homes_root: if common.no_discover_homes {
            None
        } else {
            common.discover_homes.clone()
        },
        tmp_root: if common.no_discover_tmp {
            None
        } else {
            common.discover_tmp.clone()
        },
        destination_dir: destination.parent().unwrap_or(destination).to_path_buf(),
    })
}

fn retention_days(common: &CommonArgs) -> Option<u32> {
    if common.no_retention_cutoff {
        None
    } else {
        Some(common.retention_days)
    }
}

/// The observability cutoff shares the global disable switch but keeps the
/// stream's own default window (7 days in v1).
fn observability_retention_days(common: &CommonArgs) -> Option<u32> {
    if common.no_retention_cutoff {
        None
    } else {
        Some(common.observability_retention_days)
    }
}

fn is_explicit(dir: &Path, explicit: &[std::path::PathBuf]) -> bool {
    explicit.iter().any(|candidate| {
        candidate == dir
            || (std::fs::canonicalize(candidate)
                .ok()
                .is_some_and(|canonical| canonical == dir))
    })
}

fn emit<T: serde::Serialize>(report: &T, human: &str, json: bool) {
    if json {
        match serde_json::to_string_pretty(report) {
            Ok(rendered) => println!("{rendered}"),
            Err(err) => eprintln!("warning: could not render JSON: {err}"),
        }
    } else {
        print!("{human}");
    }
}
