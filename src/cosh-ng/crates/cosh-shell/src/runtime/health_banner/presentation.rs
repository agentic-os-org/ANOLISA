use crate::config::guide::{
    Claim, ClaimDecision, ClaimRequest, GuideSnapshot, GuideStore, HealthExposure, InviteKind,
    RuntimeState,
};
use crate::diagnostics::health::HealthScanReport;
use crate::ui::{guidance, RatatuiInlineRenderer};
use crate::Language;

pub(super) struct Prepared {
    facts: String,
    facts_help_only: String,
    show_entry: bool,
    snapshot: GuideSnapshot,
    invitation: Option<(Claim, String)>,
}

impl Prepared {
    pub(super) fn final_text(&self, store: &GuideStore, runtime: Option<&RuntimeState>) -> &str {
        if let Some((claim, text)) = &self.invitation {
            if store.validate_claim(runtime, claim) == Ok(true) {
                return text;
            }
        }
        if self.show_entry && store.guidance_still_enabled(runtime, &self.snapshot) == Ok(true) {
            return &self.facts;
        }
        &self.facts_help_only
    }
}

pub(super) fn prepare(
    report: &HealthScanReport,
    renderer: &RatatuiInlineRenderer,
    state: (&GuideStore, Option<&RuntimeState>, &GuideSnapshot),
    language: Language,
    ai_enabled: bool,
    now_ms: u64,
) -> Prepared {
    let (store, runtime, snapshot) = state;
    let show_facts = snapshot.preferences.banner_enabled
        && !report.facts.is_empty()
        && report
            .login_config
            .as_ref()
            .is_none_or(|config| config.health.enabled);
    let show_entry = ai_enabled && snapshot.preferences.guide_enabled;
    let body = guidance::render_health_with_entry(
        renderer,
        report,
        show_facts,
        guidance::Target::Bash,
        None,
        show_entry,
    )
    .unwrap_or_default();
    let facts = if body.is_empty() {
        body
    } else {
        format!("\n{body}")
    };
    let facts_help_only = if show_entry {
        let body = guidance::render_health_with_entry(
            renderer,
            report,
            show_facts,
            guidance::Target::Bash,
            None,
            false,
        )
        .unwrap_or_default();
        if body.is_empty() {
            body
        } else {
            format!("\n{body}")
        }
    } else {
        facts.clone()
    };
    let mut prepared = Prepared {
        facts,
        facts_help_only,
        show_entry,
        snapshot: snapshot.clone(),
        invitation: None,
    };
    if let Some(path) = crate::diagnostics::health::suppression::episode::ledger_path() {
        if let Some(mut ledger) =
            crate::diagnostics::health::suppression::episode::load_or_default(&path)
        {
            let signal =
                crate::diagnostics::health::suppression::episode::signals_from_report(report);
            crate::diagnostics::health::suppression::episode::sync(&mut ledger, &signal, now_ms);
            if let Err(error) =
                crate::diagnostics::health::suppression::episode::write(&path, &ledger)
            {
                tracing::warn!(
                    target: "cosh_health",
                    path = %path.display(),
                    error = ?error,
                    "episode ledger write failed; recovery records and counter baselines may not persist across logins"
                );
            }
        }
    }
    if !ai_enabled || !snapshot.preferences.guide_enabled {
        return prepared;
    }
    let had_candidate = !report.try_items.is_empty();
    let candidate = if snapshot.preferences.banner_enabled {
        guidance::health_candidate(report, language)
    } else {
        None
    };
    let (kind, invitation) = if let Some(candidate) = candidate {
        (InviteKind::LoginHealth, candidate)
    } else if !show_facts && (!snapshot.preferences.banner_enabled || !had_candidate) {
        (InviteKind::LoginEntry, guidance::login_entry(language))
    } else {
        return prepared;
    };
    let Some(text) = guidance::render_health_with_entry(
        renderer,
        report,
        show_facts,
        guidance::Target::Bash,
        Some(&invitation),
        show_entry,
    ) else {
        return prepared;
    };
    if text.len() >= crate::diagnostics::health::banner_policy::MAX_TEXT_BYTES {
        return prepared;
    }
    let text = if text.is_empty() {
        text
    } else {
        format!("\n{text}")
    };
    let session = report.scan_id.as_str();
    let request = ClaimRequest {
        kind,
        host: report.host.as_deref().unwrap_or("local"),
        problem: &invitation.problem,
        session,
    };
    let decision = if kind == InviteKind::LoginHealth {
        match invitation.evidence.as_deref() {
            Some(evidence) => store.claim_health_version(
                runtime,
                snapshot,
                request,
                now_ms,
                HealthExposure {
                    evidence,
                    event: invitation
                        .event
                        .as_ref()
                        .map(|(domain, value)| (*domain, value.as_str())),
                    severity: invitation.severity,
                    legacy_problems: &invitation.legacy_problems,
                    episode: invitation.episode.clone(),
                },
            ),
            None => store.claim(runtime, snapshot, request, now_ms),
        }
    } else {
        store.claim(runtime, snapshot, request, now_ms)
    };
    if let Ok(ClaimDecision::Claimed(claim)) = decision {
        prepared.invitation = Some((claim, text));
    }
    prepared
}
