#[cfg(test)]
use crate::diagnostics::health::suppression::metrics_for_report;
use crate::diagnostics::health::HealthScanReport;
#[cfg(test)]
use crate::diagnostics::health::{HealthSuppressionStore, SuppressionDecision};
use crate::ui::guidance::Invitation;
use crate::Language;

#[cfg(test)]
fn filtered_report(
    report: &HealthScanReport,
    suppression: &HealthSuppressionStore,
    host: &str,
    now_ms: u64,
) -> HealthScanReport {
    let metrics = metrics_for_report(report);
    let mut filtered = report.clone();
    filtered.try_items.retain(|item| {
        report
            .findings
            .iter()
            .find(|finding| finding.id == item.finding_id)
            .is_some_and(|finding| {
                suppression.should_show(host, finding, &metrics, now_ms)
                    == SuppressionDecision::Show
            })
    });
    filtered
}

#[cfg(test)]
fn selected_invitation(
    report: &HealthScanReport,
    language: Language,
    suppression: &HealthSuppressionStore,
    host: &str,
    now_ms: u64,
) -> Option<Invitation> {
    crate::ui::guidance::health_candidate_for(
        &filtered_report(report, suppression, host, now_ms),
        language,
        crate::ui::guidance::Target::Cosh,
    )
}

pub(super) struct PreparedHealth {
    pub(super) invitation: Option<Invitation>,
    pub(super) candidates: Vec<crate::diagnostics::health::HealthTryItem>,
    guide: Option<(
        crate::config::guide::GuideStore,
        Option<crate::config::guide::RuntimeState>,
        crate::config::guide::GuideSnapshot,
    )>,
}

impl PreparedHealth {
    pub(super) fn empty() -> Self {
        Self {
            invitation: None,
            candidates: Vec::new(),
            guide: None,
        }
    }

    fn valid(&self) -> bool {
        let Some((store, runtime, snapshot)) = &self.guide else {
            return false;
        };
        let runtime_enabled = runtime
            .as_ref()
            .is_none_or(|runtime| matches!(runtime.read(), Ok(Some(state)) if state.enabled));
        runtime_enabled
            && store.snapshot(runtime.as_ref()).is_ok_and(|current| {
                current.preferences.guide_enabled && current.generation == snapshot.generation
            })
    }
}

pub(super) fn prepare_session(
    report: &HealthScanReport,
    language: Language,
    allowed: bool,
) -> PreparedHealth {
    if !allowed {
        return PreparedHealth::empty();
    }
    if crate::ui::guidance::health_candidate_for(
        report,
        language,
        crate::ui::guidance::Target::Cosh,
    )
    .is_none()
    {
        return PreparedHealth::empty();
    }
    let Ok((store, runtime)) = crate::runtime::health_banner::open_guide_state() else {
        return PreparedHealth::empty();
    };
    let Ok(snapshot) = store.snapshot(runtime.as_ref()) else {
        return PreparedHealth::empty();
    };
    if !snapshot.preferences.guide_enabled {
        return PreparedHealth::empty();
    }
    let mut candidates = Vec::new();
    for item in crate::diagnostics::health::sorted_try_items(report) {
        let mut selected = report.clone();
        selected
            .try_items
            .retain(|candidate| candidate.id == item.id);
        if crate::ui::guidance::health_candidate_for(
            &selected,
            language,
            crate::ui::guidance::Target::Cosh,
        )
        .is_some()
        {
            if let Some(finding) = report
                .findings
                .iter()
                .find(|finding| finding.id == item.finding_id)
            {
                candidates.push(crate::diagnostics::health::suppression::claim::Candidate {
                    id: item.id.clone(),
                    finding: finding.clone(),
                });
            }
        }
    }
    let Some(claim) = crate::diagnostics::health::suppression::claim::claim(
        report,
        candidates,
        &snapshot.generation,
        runtime.is_some(),
    ) else {
        return PreparedHealth::empty();
    };
    let mut filtered = report.clone();
    filtered
        .try_items
        .retain(|item| claim.eligible.contains(&item.id));
    let mut primary = filtered.clone();
    primary.try_items.retain(|item| item.id == claim.selected);
    let invitation = crate::ui::guidance::health_candidate_for(
        &primary,
        language,
        crate::ui::guidance::Target::Cosh,
    );

    PreparedHealth {
        invitation,
        candidates: filtered.try_items,
        guide: Some((store, runtime, snapshot)),
    }
}

pub(super) fn write_session_health<W: std::io::Write>(
    prepared: &PreparedHealth,
    report: &HealthScanReport,
    renderer: &crate::ui::RatatuiInlineRenderer,
    output: &mut W,
) -> std::io::Result<bool> {
    let facts = crate::ui::guidance::render_health(
        renderer,
        report,
        true,
        crate::ui::guidance::Target::Cosh,
        None,
    )
    .unwrap_or_default();
    let invited = prepared.invitation.as_ref().and_then(|invitation| {
        crate::ui::guidance::render_health(
            renderer,
            report,
            true,
            crate::ui::guidance::Target::Cosh,
            Some(invitation),
        )
    });
    let visible = invited.is_some() && prepared.valid();
    output.write_all(if visible {
        invited.as_deref().unwrap_or(&facts).as_bytes()
    } else {
        facts.as_bytes()
    })?;
    output.flush()?;
    Ok(visible)
}

pub(super) fn discard_health_ghost(
    state: &mut crate::runtime::state::InlineState,
    problem: Option<&str>,
) {
    use crate::runtime::state::PendingInputGhostBinding;
    let mut health_ids = state
        .pending_prompt_suggestion_bindings
        .iter()
        .filter(|(_, binding)| matches!(binding, PendingInputGhostBinding::Health(_)))
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    if let Some(problem) = problem {
        health_ids.push(problem.to_string());
    }
    state
        .pending_prompt_suggestion_bindings
        .retain(|_, binding| !matches!(binding, PendingInputGhostBinding::Health(_)));
    let crate::raw_input::PromptGhostRoute::AgentSelection { candidates, active } =
        &state.pending_input_ghost_route
    else {
        return;
    };
    let active_id = candidates
        .get(*active)
        .map(|candidate| candidate.suggestion_id.clone());
    let remaining = candidates
        .iter()
        .filter(|candidate| !health_ids.contains(&candidate.suggestion_id))
        .cloned()
        .collect::<Vec<_>>();
    let active = active_id
        .and_then(|id| {
            remaining
                .iter()
                .position(|candidate| candidate.suggestion_id == id)
        })
        .unwrap_or(0);
    if let Some(selected) = remaining.get(active) {
        state.pending_input_ghost = Some(selected.text.clone());
        state.pending_input_ghost_binding = state
            .pending_prompt_suggestion_bindings
            .get(&selected.suggestion_id)
            .cloned();
        state.pending_input_ghost_route = crate::raw_input::PromptGhostRoute::AgentSelection {
            candidates: remaining,
            active,
        };
    } else {
        state.pending_input_ghost = None;
        state.pending_input_ghost_binding = None;
        state.pending_input_ghost_route = crate::raw_input::PromptGhostRoute::NativeShell;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::health::suppression::metrics_for_report;
    use crate::diagnostics::health::{
        HealthFinding, HealthFindingCategory, HealthMessageId, HealthSeverity, HealthTryItem,
        HealthTryKind,
    };
    use std::os::unix::fs::PermissionsExt;

    fn report(severity: HealthSeverity) -> HealthScanReport {
        let mut report = HealthScanReport::new("worker-pool", 1000);
        report.overall_severity = severity;
        report.findings.push(HealthFinding {
            id: "J06".to_string(),
            severity,
            category: HealthFindingCategory::RootCause,
            title_id: HealthMessageId::HealthFindingMemoryAvailableLow,
            detail_id: None,
            detail_args: Default::default(),
            evidence_fact_ids: vec![],
            suggested_try_ids: vec!["worker-memory".to_string()],
        });
        report.try_items.push(HealthTryItem {
            id: "worker-memory".to_string(),
            label_id: HealthMessageId::HealthTryAnalyzeMemoryPressure,
            label_args: Default::default(),
            prompt_id: Some(HealthMessageId::HealthPromptAnalyzeMemoryPressure),
            prompt_args: Default::default(),
            kind: HealthTryKind::AskAgent,
            command: None,
            reason_id: HealthMessageId::HealthTryReasonMemoryLow,
            reason_args: Default::default(),
            score: 100,
            finding_id: "J06".to_string(),
        });
        report
    }

    #[test]
    fn session_health_consumes_the_worker_candidate_without_regenerating_the_pool() {
        let report = report(HealthSeverity::Warning);
        let original = report.clone();
        let invitation = selected_invitation(
            &report,
            Language::EnUs,
            &HealthSuppressionStore::default(),
            "host",
            1000,
        )
        .unwrap();
        assert_eq!(invitation.problem, "worker-memory");
        assert_eq!(report, original);
    }

    #[test]
    fn session_health_retains_warning_four_hour_and_critical_thirty_minute_ttls() {
        for (severity, ttl) in [
            (HealthSeverity::Warning, 4 * 60 * 60 * 1000),
            (HealthSeverity::Critical, 30 * 60 * 1000),
        ] {
            let report = report(severity);
            let mut suppression = HealthSuppressionStore::default();
            suppression.record_shown(
                "host",
                &report.findings[0],
                metrics_for_report(&report),
                1000,
            );
            let dir = tempfile::tempdir().unwrap();
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            let path = dir
                .path()
                .canonicalize()
                .unwrap()
                .join("health-suppression");
            suppression.write_to_path(&path).unwrap();
            let suppression = HealthSuppressionStore::load_from_path(&path);
            assert!(
                selected_invitation(&report, Language::EnUs, &suppression, "host", 1001).is_none()
            );
            assert!(
                selected_invitation(&report, Language::EnUs, &suppression, "host", 1000 + ttl)
                    .is_some()
            );
        }
    }

    #[test]
    fn session_health_off_after_preparation_keeps_facts_and_discards_the_question() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = crate::config::guide::GuideStore::open(root.join("guide")).unwrap();
        let snapshot = store.snapshot(None).unwrap();
        let report = report(HealthSeverity::Warning);
        let original = report.clone();
        let invitation = crate::ui::guidance::health_candidate_for(
            &report,
            Language::EnUs,
            crate::ui::guidance::Target::Cosh,
        )
        .unwrap();
        let prepared = PreparedHealth {
            invitation: Some(invitation),
            candidates: report.try_items.clone(),
            guide: Some((store, None, snapshot)),
        };
        prepared
            .guide
            .as_ref()
            .unwrap()
            .0
            .set_guide(None, false)
            .unwrap();
        let mut output = Vec::new();
        assert!(!write_session_health(
            &prepared,
            &report,
            &crate::ui::RatatuiInlineRenderer::plain_with_width(80),
            &mut output
        )
        .unwrap());
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("Health check"));
        assert!(!text.contains("Try asking"));
        assert!(!text.contains("cosh --ask"));
        assert_eq!(report, original);
    }

    #[test]
    fn discarded_health_ghost_preserves_the_existing_personal_route() {
        use crate::raw_input::{PromptGhostCandidate, PromptGhostRoute};
        let mut state = crate::runtime::state::InlineState {
            pending_input_ghost: Some("health question".to_string()),
            pending_input_ghost_route: PromptGhostRoute::AgentSelection {
                candidates: vec![
                    PromptGhostCandidate {
                        text: "health question".to_string(),
                        suggestion_id: "health".to_string(),
                    },
                    PromptGhostCandidate {
                        text: "personal question".to_string(),
                        suggestion_id: "personal".to_string(),
                    },
                    PromptGhostCandidate {
                        text: "second health".into(),
                        suggestion_id: "health2".into(),
                    },
                ],
                active: 0,
            },
            ..Default::default()
        };
        for id in ["health", "health2"] {
            state.pending_prompt_suggestion_bindings.insert(
                id.into(),
                crate::runtime::state::PendingInputGhostBinding::Health(
                    crate::types::AgentContextBinding::StartupHealthFollowUp,
                ),
            );
        }
        state.pending_prompt_suggestion_bindings.insert(
            "personal".into(),
            crate::runtime::state::PendingInputGhostBinding::Personal(
                crate::recommendation::personal_feedback::FrozenPromptBinding {
                    candidate_id: "personal".into(),
                    task_ref: "task".into(),
                    original_prompt: "personal question".into(),
                    source: crate::recommendation::personal_model::CandidateSource::RecentTask,
                    suppression_key: "personal".into(),
                    profile_generation: 7,
                    intent_lifecycle_id: "intent".into(),
                },
            ),
        );
        discard_health_ghost(&mut state, Some("health"));
        assert_eq!(
            state.pending_input_ghost.as_deref(),
            Some("personal question")
        );
        assert_eq!(state.pending_prompt_suggestion_bindings.len(), 1);
        assert!(
            matches!(&state.pending_input_ghost_binding, Some(crate::runtime::state::PendingInputGhostBinding::Personal(binding)) if binding.profile_generation == 7 && binding.candidate_id == "personal")
        );
        assert!(
            matches!(&state.pending_input_ghost_route, PromptGhostRoute::AgentSelection {candidates,..} if candidates.len()==1 && candidates[0].suggestion_id=="personal")
        );
    }
    #[test]
    fn session_health_uses_the_next_unsuppressed_worker_candidate() {
        let mut report = report(HealthSeverity::Warning);
        let mut finding = report.findings[0].clone();
        finding.id = "J07".into();
        finding.title_id = HealthMessageId::HealthFindingSwapPressure;
        let mut item = report.try_items[0].clone();
        item.id = "worker-swap".into();
        item.finding_id = finding.id.clone();
        item.prompt_id = Some(HealthMessageId::HealthPromptCheckSwapPressure);
        report.findings.push(finding);
        report.try_items.push(item);
        let original = report.clone();
        let mut suppression = HealthSuppressionStore::default();
        suppression.record_shown(
            "host",
            &report.findings[0],
            metrics_for_report(&report),
            1000,
        );
        let invitation =
            selected_invitation(&report, Language::EnUs, &suppression, "host", 1001).unwrap();
        assert_eq!(invitation.problem, "worker-swap");
        assert_eq!(report, original);
    }

    #[test]
    fn session_health_preparation_keeps_the_full_ghost_pool_while_showing_one_question() {
        let _lock = crate::diagnostics::test_env::env_guard();
        struct Restore(Vec<(&'static str, Option<std::ffi::OsString>)>);
        impl Drop for Restore {
            fn drop(&mut self) {
                for (key, value) in &self.0 {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
        let names = [
            "HOME",
            "XDG_RUNTIME_DIR",
            "COSH_SHELL_ISOLATED",
            "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
            "TERM",
        ];
        let _restore = Restore(names.map(|key| (key, std::env::var_os(key))).into());
        let dir = tempfile::tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let home = dir.path().canonicalize().unwrap();
        std::env::set_var("HOME", &home);
        std::env::set_var(
            "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
            home.join("suppression"),
        );
        std::env::set_var("TERM", "xterm-256color");
        std::env::remove_var("XDG_RUNTIME_DIR");
        std::env::remove_var("COSH_SHELL_ISOLATED");
        let mut report = report(HealthSeverity::Warning);
        for (id, title, prompt) in [
            (
                "swap",
                HealthMessageId::HealthFindingSwapPressure,
                HealthMessageId::HealthPromptCheckSwapPressure,
            ),
            (
                "disk",
                HealthMessageId::HealthFindingDiskHigh,
                HealthMessageId::HealthPromptInspectDiskUsage,
            ),
        ] {
            let mut finding = report.findings[0].clone();
            finding.id = id.into();
            finding.title_id = title;
            let mut item = report.try_items[0].clone();
            item.id = id.into();
            item.finding_id = id.into();
            item.prompt_id = Some(prompt);
            report.findings.push(finding);
            report.try_items.push(item);
        }
        let original = report.clone();
        let mut state = crate::runtime::state::InlineState {
            language: Language::EnUs,
            ..Default::default()
        };
        state.startup_health.report = Some(report);
        let prepared = super::super::prepare_startup_suggestions(&mut state, "/fixture");
        assert_eq!(prepared.candidates.len(), 3);
        assert!(
            matches!(&state.pending_input_ghost_route, crate::raw_input::PromptGhostRoute::AgentSelection { candidates, .. } if candidates.len()==3)
        );
        assert_eq!(state.pending_prompt_suggestion_bindings.len(), 3);
        let mut output = Vec::new();
        assert!(write_session_health(
            &prepared.health,
            &original,
            &crate::ui::RatatuiInlineRenderer::plain_with_width(100),
            &mut output
        )
        .unwrap());
        let text = String::from_utf8(output).unwrap();
        assert_eq!(text.matches("Try asking:").count(), 1);
        assert!(!text.contains("Check whether swap pressure"));
        assert_eq!(state.startup_health.report.as_ref(), Some(&original));
    }
}
