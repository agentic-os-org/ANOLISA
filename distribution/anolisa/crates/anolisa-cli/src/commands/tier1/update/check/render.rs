//! Rendering for `update --check`: JSON envelope, short MOTD, and the human
//! report. Kept separate from the detection logic in the parent module so the
//! output vocabulary lives in one place.

use super::super::UpdateArgs;
use super::{
    ACTION_ERROR, ACTION_INSTALL, ACTION_NOOP, ACTION_RECONCILE, ACTION_UNSUPPORTED_RPM,
    ACTION_UPDATE, CHECK_COMMAND, CliCheck, ComponentCheck, UpdateCheckReport,
};
use crate::color::Palette;
use crate::context::CliContext;
use crate::response;

/// Dispatch to the JSON, MOTD, or human renderer per the active flags. JSON
/// wins over `--motd` so `--check --json --motd` still yields the full envelope.
pub(super) fn render_report(ctx: &CliContext, args: &UpdateArgs, report: &UpdateCheckReport) {
    if ctx.json {
        // A plain Serialize struct never fails here; ignore the Result so a
        // successful check is not misreported.
        let _ = response::render_json(CHECK_COMMAND, report);
        return;
    }
    if args.motd {
        render_motd(ctx, report);
        return;
    }
    render_human(ctx, report);
}

/// Short, stable MOTD summary. Silent when there is nothing to do so a login
/// banner stays quiet on an up-to-date host.
pub(super) fn render_motd(ctx: &CliContext, report: &UpdateCheckReport) {
    if ctx.quiet {
        return;
    }
    if let Some(text) = build_motd(report) {
        println!("{text}");
    }
}

/// Build the MOTD text, or `None` when no upgrade action is required.
pub(super) fn build_motd(report: &UpdateCheckReport) -> Option<String> {
    let summary = &report.summary;
    if summary.updates == 0 && summary.reconciliations == 0 && summary.missing_defaults == 0 {
        return None;
    }
    let mut parts = Vec::new();
    if summary.updates > 0 {
        parts.push(format!(
            "{} component{} can be upgraded",
            summary.updates,
            plural(summary.updates)
        ));
    }
    if summary.missing_defaults > 0 {
        parts.push(format!(
            "{} new default component{} can be installed",
            summary.missing_defaults,
            plural(summary.missing_defaults)
        ));
    }
    if summary.reconciliations > 0 {
        parts.push(format!(
            "{} component{} requires state reconciliation",
            summary.reconciliations,
            plural(summary.reconciliations)
        ));
    }
    let headline = if summary.updates == 0 && summary.missing_defaults == 0 {
        "ANOLISA state reconciliation is required."
    } else {
        "ANOLISA toolchain update is available."
    };
    Some(format!(
        "{headline}\n{}.\nRun: \"sudo anolisa upgrade\" to apply, or \"anolisa update --check\" for details",
        parts.join("; ")
    ))
}

fn render_human(ctx: &CliContext, report: &UpdateCheckReport) {
    if ctx.quiet {
        return;
    }
    let color = Palette::new(ctx.no_color);

    let mut header = format!("update check (backend: {}", report.backend);
    if let Some(target) = &report.target {
        header.push_str(&format!(", target: {target}"));
    }
    header.push(')');
    println!("{}", color.command(header));

    render_cli_line(&report.cli, &color);
    for component in &report.components {
        render_component_line(component, &color);
    }

    let summary = &report.summary;
    println!(
        "{} {} update(s), {} reconciliation(s), {} new default(s), {} unsupported, {} error(s)",
        color.label("summary:"),
        summary.updates,
        summary.reconciliations,
        summary.missing_defaults,
        summary.unsupported,
        summary.errors,
    );
}

fn render_cli_line(cli: &CliCheck, color: &Palette) {
    let label = color.label("CLI:");
    match cli.action.as_str() {
        ACTION_UPDATE => println!(
            "{label} {} {} → {} {}",
            cli.package.as_deref().unwrap_or("anolisa"),
            cli.installed.as_deref().unwrap_or("-"),
            cli.available.as_deref().unwrap_or("-"),
            color.warn("(update)"),
        ),
        ACTION_NOOP => println!(
            "{label} {} {} {}",
            cli.package.as_deref().unwrap_or("anolisa"),
            cli.installed.as_deref().unwrap_or("-"),
            color.ok("(up to date)"),
        ),
        ACTION_ERROR => println!(
            "{label} {} {}",
            cli.package.as_deref().unwrap_or("anolisa"),
            color.warn(format!(
                "(error: {})",
                cli.error.as_deref().unwrap_or("unknown")
            )),
        ),
        _ => println!(
            "{label} {}",
            color.muted(format!(
                "not RPM-managed ({})",
                cli.error.as_deref().unwrap_or("use `anolisa update self`")
            )),
        ),
    }
}

fn render_component_line(component: &ComponentCheck, color: &Palette) {
    let ownership = component.ownership.as_deref().unwrap_or("");
    let meta = if ownership.is_empty() {
        String::new()
    } else {
        color.muted(format!(" ({ownership})"))
    };
    match component.action.as_str() {
        ACTION_UPDATE => println!(
            "  {}{} {} → {} {}",
            component.component,
            meta,
            component.installed.as_deref().unwrap_or("-"),
            component.available.as_deref().unwrap_or("-"),
            color.warn("(update)"),
        ),
        ACTION_INSTALL => println!(
            "  {} {}",
            component.component,
            color.warn("(new default — can be installed)"),
        ),
        ACTION_RECONCILE => println!(
            "  {}{} {}",
            component.component,
            meta,
            color.warn("(state reconciliation required)"),
        ),
        ACTION_UNSUPPORTED_RPM => println!(
            "  {}{} {}",
            component.component,
            meta,
            color.muted("(not in RPM upgrade scope)"),
        ),
        ACTION_ERROR => println!(
            "  {}{} {}",
            component.component,
            meta,
            color.warn(format!(
                "(error: {})",
                component.error.as_deref().unwrap_or("unknown")
            )),
        ),
        _ => println!(
            "  {}{} {}",
            component.component,
            meta,
            color.ok("(up to date)"),
        ),
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
