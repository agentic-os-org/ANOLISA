//! Command adapter and compatibility renderer for `anolisa update self`.

use serde::Serialize;

use anolisa_core::execution::ExecutionIntent;
use anolisa_core::self_update::{self as core_self_update, ProgressFn};

use crate::color::Palette;
use crate::context::CliContext;
use crate::response::{self, CliError};

pub(super) mod application;

use application::{SelfUpdateApplicationOutcome, SelfUpdateApplied, SelfUpdateRequest};

const CLI_CHANGELOG_URL: &str = "https://agentic-os.sh/#anolisa-cli-changelog";

/// Executes CLI self-update through the typed application boundary.
///
/// Also called from `anolisa self update` as a convenience alias.
pub(in crate::commands) fn handle(ctx: &CliContext) -> Result<(), CliError> {
    let endpoint_url = core_self_update::update_url();
    let intent = if ctx.dry_run {
        ExecutionIntent::Plan
    } else {
        ExecutionIntent::Apply
    };
    let progress_cb: Option<ProgressFn> = if !ctx.json && !ctx.quiet {
        Some(Box::new(move |downloaded: u64, total: Option<u64>| {
            render_progress(downloaded, total);
        }))
    } else {
        None
    };

    let result = application::run(
        SelfUpdateRequest {
            endpoint_url: &endpoint_url,
            current_version: env!("CARGO_PKG_VERSION"),
            intent,
        },
        ctx,
        progress_cb.as_ref(),
    );

    // Clear the progress line before any warning, success, or terminal error.
    if progress_cb.is_some() {
        eprint!("\r\x1b[2K");
    }

    let outcome = match result {
        Ok(outcome) => outcome,
        Err(failure) => {
            render_warnings(&failure.warnings);
            return Err(failure.error);
        }
    };
    if let SelfUpdateApplicationOutcome::Applied {
        outcome: command_outcome,
        ..
    } = &outcome
    {
        render_warnings(command_outcome.warnings());
    }

    if ctx.json {
        return response::render_json("update self", build_json_data(&outcome));
    }
    if ctx.quiet {
        return Ok(());
    }

    render_human(&outcome, ctx);
    Ok(())
}

fn render_warnings(warnings: &[String]) {
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
}

fn render_human(outcome: &SelfUpdateApplicationOutcome, ctx: &CliContext) {
    let color = Palette::new(ctx.no_color);
    match outcome {
        SelfUpdateApplicationOutcome::AlreadyLatest { version } => {
            println!(
                "{} anolisa {} is already the latest version",
                color.ok("✓"),
                version
            );
        }
        SelfUpdateApplicationOutcome::Preview { from, to } => {
            println!("{} update available: {} → {}", color.warn("⬆"), from, to);
            println!("  run without --dry-run to apply");
        }
        SelfUpdateApplicationOutcome::Applied {
            result: SelfUpdateApplied::Binary { from, to },
            ..
        } => {
            println!("{} anolisa updated: {} → {}", color.ok("✓"), from, to);
            println!("  view the changelog at {}", color.path(CLI_CHANGELOG_URL));
            eprintln!(
                "  {} signature verification not yet implemented; \
                 update trust relies on HTTPS only",
                color.warn("⚠")
            );
        }
        SelfUpdateApplicationOutcome::Applied {
            result:
                SelfUpdateApplied::RpmPackage {
                    from,
                    to,
                    package,
                    before_version,
                    after_version,
                },
            ..
        } => {
            println!(
                "{} delegated anolisa self-update to dnf package {}",
                color.ok("✓"),
                color.path(package)
            );
            println!("  release manifest advertises {to} (running binary was {from})");
            render_rpm_version_observation(before_version.as_deref(), after_version.as_deref());
        }
    }
}

fn render_rpm_version_observation(before_version: Option<&str>, after_version: Option<&str>) {
    match (before_version, after_version) {
        (Some(before), Some(after)) if before != after => {
            println!("  installed RPM version changed: {before} → {after}");
        }
        (Some(version), Some(_)) => {
            println!("  installed RPM version remains {version}");
        }
        (Some(before), None) => {
            println!(
                "  installed RPM version before dnf was {before}; after dnf was not confirmed"
            );
        }
        (None, Some(after)) => {
            println!("  installed RPM version after dnf: {after}");
        }
        (None, None) => {
            println!("  installed RPM version was not confirmed after dnf");
        }
    }
}

fn render_progress(downloaded: u64, total: Option<u64>) {
    match total {
        Some(total) if total > 0 => {
            let pct = (downloaded as f64 / total as f64 * 100.0).min(100.0);
            eprint!(
                "\r  downloading ... {:.1} / {:.1} MiB ({:.0}%)",
                downloaded as f64 / 1_048_576.0,
                total as f64 / 1_048_576.0,
                pct,
            );
        }
        _ => {
            eprint!(
                "\r  downloading ... {:.1} MiB",
                downloaded as f64 / 1_048_576.0,
            );
        }
    }
}

#[derive(Serialize)]
struct SelfUpdateData {
    current_version: String,
    latest_version: String,
    update_available: bool,
    updated: bool,
    apply_mode: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    package: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rpm_version_before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rpm_version_after: Option<String>,
}

fn build_json_data(outcome: &SelfUpdateApplicationOutcome) -> SelfUpdateData {
    match outcome {
        SelfUpdateApplicationOutcome::AlreadyLatest { version } => SelfUpdateData {
            current_version: version.clone(),
            latest_version: version.clone(),
            update_available: false,
            updated: false,
            apply_mode: "none",
            package: None,
            rpm_version_before: None,
            rpm_version_after: None,
        },
        SelfUpdateApplicationOutcome::Preview { from, to } => SelfUpdateData {
            current_version: from.clone(),
            latest_version: to.clone(),
            update_available: true,
            updated: false,
            apply_mode: "none",
            package: None,
            rpm_version_before: None,
            rpm_version_after: None,
        },
        SelfUpdateApplicationOutcome::Applied {
            result: SelfUpdateApplied::Binary { from, to },
            ..
        } => SelfUpdateData {
            current_version: from.clone(),
            latest_version: to.clone(),
            update_available: true,
            updated: true,
            apply_mode: "binary",
            package: None,
            rpm_version_before: None,
            rpm_version_after: None,
        },
        SelfUpdateApplicationOutcome::Applied {
            result:
                SelfUpdateApplied::RpmPackage {
                    from,
                    to,
                    package,
                    before_version,
                    after_version,
                },
            ..
        } => SelfUpdateData {
            current_version: from.clone(),
            latest_version: to.clone(),
            update_available: true,
            updated: before_version
                .as_ref()
                .zip(after_version.as_ref())
                .is_some_and(|(before, after)| before != after),
            apply_mode: "rpm_package",
            package: Some(package.clone()),
            rpm_version_before: before_version.clone(),
            rpm_version_after: after_version.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use anolisa_core::execution::{CommandOutcome, CommandOutcomeStatus};

    use super::*;

    fn applied(result: SelfUpdateApplied) -> SelfUpdateApplicationOutcome {
        SelfUpdateApplicationOutcome::Applied {
            outcome: CommandOutcome::new(
                CommandOutcomeStatus::Completed,
                Some("op-update-self-1".to_string()),
                Vec::new(),
                Vec::new(),
            ),
            result,
        }
    }

    #[test]
    fn json_preview_reports_available_but_not_updated() {
        let data = build_json_data(&SelfUpdateApplicationOutcome::Preview {
            from: "0.1.0".to_string(),
            to: "0.2.0".to_string(),
        });

        assert!(data.update_available);
        assert!(!data.updated);
        assert_eq!(data.apply_mode, "none");
    }

    #[test]
    fn json_binary_apply_reports_both_true() {
        let data = build_json_data(&applied(SelfUpdateApplied::Binary {
            from: "0.1.0".to_string(),
            to: "0.2.0".to_string(),
        }));

        assert!(data.update_available);
        assert!(data.updated);
        assert_eq!(data.apply_mode, "binary");
    }

    #[test]
    fn json_rpm_noop_preserves_legacy_failed_to_move_signal() {
        let data = build_json_data(&applied(SelfUpdateApplied::RpmPackage {
            from: "0.1.0".to_string(),
            to: "0.2.0".to_string(),
            package: "anolisa".to_string(),
            before_version: Some("0.1.0".to_string()),
            after_version: Some("0.1.0".to_string()),
        }));

        assert!(data.update_available);
        assert!(!data.updated);
        assert_eq!(data.apply_mode, "rpm_package");
        assert_eq!(data.package.as_deref(), Some("anolisa"));
    }

    #[test]
    fn json_rpm_apply_reports_observed_version_change() {
        let data = build_json_data(&applied(SelfUpdateApplied::RpmPackage {
            from: "0.1.0".to_string(),
            to: "0.2.0".to_string(),
            package: "anolisa".to_string(),
            before_version: Some("0.1.0".to_string()),
            after_version: Some("0.2.0".to_string()),
        }));

        assert!(data.update_available);
        assert!(data.updated);
        assert_eq!(data.apply_mode, "rpm_package");
        assert_eq!(data.package.as_deref(), Some("anolisa"));
        assert_eq!(data.rpm_version_before.as_deref(), Some("0.1.0"));
        assert_eq!(data.rpm_version_after.as_deref(), Some("0.2.0"));
    }

    #[test]
    fn json_already_latest_reports_both_false() {
        let data = build_json_data(&SelfUpdateApplicationOutcome::AlreadyLatest {
            version: "0.1.0".to_string(),
        });

        assert!(!data.update_available);
        assert!(!data.updated);
        assert_eq!(data.apply_mode, "none");
    }
}
