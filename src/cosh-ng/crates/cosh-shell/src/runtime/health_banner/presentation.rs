use crate::config::guide::{
    Claim, ClaimDecision, ClaimRequest, GuideSnapshot, GuideStore, InviteKind, RuntimeState,
};
use crate::diagnostics::health::HealthScanReport;
use crate::ui::{guidance, RatatuiInlineRenderer};
use crate::Language;

pub(super) struct Prepared {
    facts: String,
    invitation: Option<(Claim, String)>,
}

impl Prepared {
    pub(super) fn final_text(&self, store: &GuideStore, runtime: Option<&RuntimeState>) -> &str {
        if let Some((claim, text)) = &self.invitation {
            if store.validate_claim(runtime, claim) == Ok(true) {
                return text;
            }
        }
        &self.facts
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
    let body = guidance::render_health(renderer, report, show_facts, guidance::Target::Bash, None)
        .unwrap_or_default();
    let facts = body;
    let mut prepared = Prepared {
        facts,
        invitation: None,
    };
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
    } else if !snapshot.preferences.banner_enabled || !had_candidate {
        (InviteKind::LoginEntry, guidance::login_entry(language))
    } else {
        return prepared;
    };
    let Some(text) = guidance::render_health(
        renderer,
        report,
        show_facts,
        guidance::Target::Bash,
        Some(&invitation),
    ) else {
        return prepared;
    };
    if text.len() > 16 * 1024 {
        return prepared;
    }
    let session = report.scan_id.as_str();
    let request = ClaimRequest {
        kind,
        host: report.host.as_deref().unwrap_or("local"),
        problem: &invitation.problem,
        session,
    };
    if let Ok(ClaimDecision::Claimed(claim)) = store.claim(runtime, snapshot, request, now_ms) {
        prepared.invitation = Some((claim, text));
    }
    prepared
}
