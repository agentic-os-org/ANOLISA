//! `/health` slash command: render the shared on-demand doctor report as an
//! inline card. Uses the same [`run_doctor_report`] engine and status model as
//! the `cosh-shell doctor` CLI, so both entry points report identical checks.
//!
//! On top of the file-based report, `/health` is the only diagnostic entry
//! point running inside a live session, so it additionally renders a live
//! block with transient facts the process can only observe while the session
//! is alive: core liveness, session recovery state, and the shell routing
//! facts (issue #3055). Live findings stay out of the doctor report so
//! `diagnostics export` keeps its file-only evidence contract; the routing
//! facts reach exports through the run-registry snapshot instead.

use crate::adapter::{AdapterInstance, CoreLiveness};
use crate::config::CoshConfig;
use crate::diagnostics::doctor::run_doctor_report;
use crate::diagnostics::health::{finding_remediation, HealthSeverity};
use crate::diagnostics::run_registry;
use crate::runtime::prelude::*;

pub(crate) fn render_health_command<W: Write>(
    state: &mut InlineState,
    shell_cwd: Option<&str>,
    adapter: &AdapterInstance,
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

    let probe = live_probe(adapter, state, &config, i18n);
    writeln!(output)?;
    writeln!(output, "{}", i18n.t(MessageId::HealthLiveSectionTitle))?;
    for line in &probe.lines {
        writeln!(output, "  {line}")?;
    }
    for finding in &probe.findings {
        writeln!(
            output,
            "[{}] {}",
            HealthSeverity::Warning.label(),
            finding.title
        )?;
        writeln!(output, "  {label}: {}", finding.remediation)?;
    }
    output.flush()
}

/// One live finding rendered under the `/health` live block.
#[derive(Debug)]
struct LiveFinding {
    title: String,
    remediation: String,
}

/// Facts and findings collected by [`live_probe`].
struct LiveProbe {
    lines: Vec<String>,
    findings: Vec<LiveFinding>,
}

/// Collects the transient live-session facts the doctor report cannot see.
///
/// The probe is strictly non-mutating: it sends a read-only registry query
/// (`auth/state`) to the live core, reads the in-process recovery and routing
/// state, and never executes a synthetic agent turn or touches shell options,
/// handlers, ZLE state, or history (issue #3055 probe contract).
fn live_probe(
    adapter: &AdapterInstance,
    state: &InlineState,
    config: &CoshConfig,
    i18n: I18n,
) -> LiveProbe {
    let core = match adapter {
        AdapterInstance::CoshCore(core) => Some(core),
        _ => None,
    };

    // 1. Core liveness: only a live registry response counts as evidence. A
    // short-lived `--registry` child would prove the binary works, not that
    // the persistent agent process answers, so `core_liveness` never falls
    // back to one.
    let liveness = core
        .map(crate::adapter::CoshCoreAdapter::core_liveness)
        .unwrap_or(CoreLiveness::NoRuntime);

    // 2. Session recovery state (in-process, no probe round-trip).
    let recovery = core.map(crate::adapter::CoshCoreAdapter::recovery_snapshot);

    // 3. Shell routing facts: effective config values plus the process-local
    // run-registry snapshot fed by the child shell (marker generation, CNF
    // handler ownership, last route decision).
    let facts = run_registry::current_routing_facts();
    let ai_enabled = config.ai_enabled;
    let assistance = state
        .assistance_control
        .as_ref()
        .map(crate::input::AssistanceControl::is_enabled)
        .or_else(|| facts.as_ref().and_then(|facts| facts.assistance_enabled));
    let marker_generation = facts
        .as_ref()
        .and_then(|facts| facts.marker_generation)
        .map(|generation| generation.to_string())
        .unwrap_or_else(|| i18n.t(MessageId::SlashValueUnavailable).to_string());
    let cnf_handler = facts
        .as_ref()
        .and_then(|facts| facts.cnf_handler.as_deref())
        .unwrap_or_else(|| i18n.t(MessageId::SlashValueUnavailable));
    let last_route = facts
        .as_ref()
        .and_then(|facts| facts.last_route.as_deref())
        .unwrap_or_else(|| i18n.t(MessageId::SlashValueUnavailable));

    let mut lines = Vec::new();
    let mut findings = Vec::new();

    match &liveness {
        CoreLiveness::Live => lines.push(i18n.t(MessageId::HealthLiveCoreAlive).to_string()),
        CoreLiveness::NoResponse(reason) => {
            lines.push(
                i18n.format(MessageId::HealthLiveCoreNoResponse, &[("reason", reason)])
                    .to_string(),
            );
            findings.push(LiveFinding {
                title: i18n
                    .format(
                        MessageId::HealthFindingCoreNoResponse,
                        &[("reason", reason)],
                    )
                    .to_string(),
                remediation: i18n
                    .t(MessageId::HealthRemediationCoreNoResponse)
                    .to_string(),
            });
        }
        CoreLiveness::NoRuntime => {
            lines.push(i18n.t(MessageId::HealthLiveCoreNoRuntime).to_string());
        }
    }

    if let Some(recovery) = &recovery {
        lines.push(
            i18n.format(
                MessageId::HealthLiveRecovery,
                &[("state", recovery.state.label())],
            )
            .to_string(),
        );
        if recovery.state == crate::adapter::SessionRecoveryState::Failed {
            findings.push(LiveFinding {
                title: i18n
                    .format(
                        MessageId::HealthFindingRecoveryFailed,
                        &[("state", recovery.state.label())],
                    )
                    .to_string(),
                remediation: i18n.t(MessageId::HealthRemediationLogs).to_string(),
            });
        }
    }

    lines.push(
        i18n.format(
            MessageId::HealthLiveRoutingFacts,
            &[
                ("ai", if ai_enabled { "on" } else { "off" }),
                (
                    "assistance",
                    match assistance {
                        Some(true) => "on",
                        Some(false) => "off",
                        None => "n/a",
                    },
                ),
                ("integration", config.shell_integration.as_str()),
                ("generation", marker_generation.as_str()),
            ],
        )
        .to_string(),
    );
    lines.push(
        i18n.format(
            MessageId::HealthLiveCnfHandler,
            &[("ownership", cnf_handler)],
        )
        .to_string(),
    );
    lines.push(
        i18n.format(MessageId::HealthLiveLastRoute, &[("route", last_route)])
            .to_string(),
    );

    // Differential diagnosis (issue #3055): a live core plus routing facts
    // that explain a fallback means the provider is fine and the routing
    // layer degraded — name that finding explicitly so users stop blaming
    // the wrong component. Only the first matching reason is reported.
    let fallback_reason = route_fallback_reason(
        &liveness,
        ai_enabled,
        assistance,
        facts
            .as_ref()
            .and_then(|facts| facts.cnf_handler.as_deref()),
    );
    if let Some(reason) = fallback_reason {
        findings.push(route_fallback_finding(reason, i18n));
    } else if matches!(liveness, CoreLiveness::Live) {
        lines.push(i18n.t(MessageId::HealthLiveRoutingHint).to_string());
    }

    LiveProbe { lines, findings }
}

/// Returns the differential-diagnosis reason when a live core coexists with
/// routing facts that explain a fallback (issue #3055): the provider is fine
/// and the routing layer degraded. `None` means no routing anomaly.
///
/// Only a live core qualifies for the differential diagnosis — a dead or
/// unresponsive core has its own finding, and a fallback without a core is
/// the expected pre-runtime state.
fn route_fallback_reason(
    liveness: &CoreLiveness,
    ai_enabled: bool,
    assistance: Option<bool>,
    cnf_handler: Option<&str>,
) -> Option<MessageId> {
    if !matches!(liveness, CoreLiveness::Live) {
        return None;
    }
    if !ai_enabled {
        return Some(MessageId::HealthLiveReasonAiDisabled);
    }
    if assistance == Some(false) {
        return Some(MessageId::HealthLiveReasonAssistanceOff);
    }
    match cnf_handler {
        Some("wrapping-user") => Some(MessageId::HealthLiveReasonUserCnf),
        Some("overridden") => Some(MessageId::HealthLiveReasonCnfOverridden),
        Some("missing") => Some(MessageId::HealthLiveReasonCnfMissing),
        _ => None,
    }
}

/// Builds the named finding for a routing compatibility fallback, carrying
/// the localized reason and the troubleshooting-guide remediation.
fn route_fallback_finding(reason: MessageId, i18n: I18n) -> LiveFinding {
    LiveFinding {
        title: i18n
            .format(
                MessageId::HealthFindingRouteFallback,
                &[("reason", i18n.t(reason))],
            )
            .to_string(),
        remediation: i18n
            .t(MessageId::HealthRemediationRouteFallback)
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::test_env::env_guard;

    fn live() -> CoreLiveness {
        CoreLiveness::Live
    }

    #[test]
    fn route_fallback_reason_matrix() {
        // Only a live core qualifies for the differential diagnosis.
        assert_eq!(
            route_fallback_reason(&CoreLiveness::NoRuntime, false, None, None),
            None
        );
        assert_eq!(
            route_fallback_reason(
                &CoreLiveness::NoResponse("timed out".to_string()),
                false,
                None,
                None
            ),
            None
        );
        // AI disabled is the strongest signal.
        assert_eq!(
            route_fallback_reason(&live(), false, None, None),
            Some(MessageId::HealthLiveReasonAiDisabled)
        );
        // Assistance (routing) off.
        assert_eq!(
            route_fallback_reason(&live(), true, Some(false), None),
            Some(MessageId::HealthLiveReasonAssistanceOff)
        );
        // A user command-not-found handler takes precedence (issue #3055
        // reproduction matrix).
        assert_eq!(
            route_fallback_reason(&live(), true, Some(true), Some("wrapping-user")),
            Some(MessageId::HealthLiveReasonUserCnf)
        );
        assert_eq!(
            route_fallback_reason(&live(), true, Some(true), Some("overridden")),
            Some(MessageId::HealthLiveReasonCnfOverridden)
        );
        assert_eq!(
            route_fallback_reason(&live(), true, Some(true), Some("missing")),
            Some(MessageId::HealthLiveReasonCnfMissing)
        );
        // No anomaly: no finding, the hint line covers wrapper coverage.
        assert_eq!(
            route_fallback_reason(&live(), true, Some(true), Some("native")),
            None
        );
        assert_eq!(route_fallback_reason(&live(), true, None, None), None);
    }

    #[test]
    fn route_fallback_finding_names_routing_not_provider() {
        let i18n = I18n::new(Language::EnUs);
        let finding = route_fallback_finding(MessageId::HealthLiveReasonUserCnf, i18n);
        assert!(
            finding
                .title
                .contains("routing compatibility fallback, not a provider failure"),
            "{}",
            finding.title
        );
        assert!(
            finding.title.contains("user command-not-found handler"),
            "{}",
            finding.title
        );
        assert!(finding.remediation.contains("troubleshooting guide"));
    }

    #[test]
    fn live_probe_reports_routing_facts_from_run_registry() {
        let _guard = env_guard();
        let home =
            std::env::temp_dir().join(format!("cosh-live-probe-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("HOME", &home);

        crate::diagnostics::run_registry::remove_shell();
        crate::diagnostics::run_registry::record_shell("zsh", "live-probe", true, "enhanced", true);
        crate::diagnostics::run_registry::update_cnf_handler("wrapping-user");
        crate::diagnostics::run_registry::update_last_route("fallback:natural_language");
        crate::diagnostics::run_registry::update_marker_generation(7);

        let adapter = AdapterInstance::Fake(crate::adapter::FakeAgentAdapter);
        let state = InlineState::default();
        let config = CoshConfig::default();
        let i18n = I18n::new(Language::EnUs);
        let probe = live_probe(&adapter, &state, &config, i18n);

        // The fake adapter has no persistent core: the liveness line reports
        // that instead of a short-child response (issue #3055 contract).
        assert!(probe
            .lines
            .iter()
            .any(|line| line.contains("no persistent runtime")));
        assert!(
            probe
                .lines
                .iter()
                .any(|line| line.contains("wrapping-user")),
            "{:?}",
            probe.lines
        );
        assert!(
            probe
                .lines
                .iter()
                .any(|line| line.contains("fallback:natural_language")),
            "{:?}",
            probe.lines
        );
        assert!(
            probe
                .lines
                .iter()
                .any(|line| line.contains("marker generation=7")),
            "{:?}",
            probe.lines
        );
        // No live core → no route-fallback finding even with a user CNF.
        assert!(
            probe
                .findings
                .iter()
                .all(|finding| !finding.title.contains("routing compatibility fallback")),
            "{:?}",
            probe.findings
        );

        crate::diagnostics::run_registry::remove_shell();
        let _ = std::fs::remove_dir_all(&home);
    }
}
