//! `/health` slash command: render the shared on-demand doctor report as an
//! inline card. Uses the same [`run_doctor_report`] engine and status model as
//! the `cosh-shell doctor` CLI, so both entry points report identical checks.

use crate::diagnostics::doctor::run_doctor_report;
use crate::diagnostics::health::finding_remediation;
use crate::runtime::prelude::*;

pub(crate) fn render_health_command<W: Write>(
    state: &mut InlineState,
    shell_cwd: Option<&str>,
    output: &mut W,
) -> std::io::Result<()> {
    let config = load_config();
    // Prefer the wrapped shell's cwd (carried by the intercept event) so hook
    // checks evaluate the directory the user actually `cd`-ed into, not the
    // parent cosh-shell launch directory. Fall back to the process cwd only
    // when the event did not carry one.
    let cwd = shell_cwd.map(std::path::PathBuf::from).unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    });
    let report = run_doctor_report(&config, &cwd);

    let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
    renderer.write_health_banner(output, HealthBannerModel::new(&report))?;

    // Match the `cosh-shell doctor` CLI by surfacing the actionable remediation
    // carried on each finding. Kept in this small slash module instead of the
    // large agent_render/health.rs renderer to avoid growing that file.
    let i18n = I18n::new(state.language);
    let label = i18n.t(MessageId::DoctorRemediationLabel);
    for finding in &report.findings {
        if let Some(text) = finding_remediation(finding, i18n) {
            writeln!(output, "  {label}: {text}")?;
        }
    }
    output.flush()
}
