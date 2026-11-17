use crate::diagnostics::health::HealthScanReport;
use crate::Language;

pub(super) struct PreparedHealth {
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
            candidates: Vec::new(),
            guide: None,
        }
    }

    pub(super) fn valid(&self) -> bool {
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
    if !allowed || crate::ui::guidance::session_health_questions(report, language).is_empty() {
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
    PreparedHealth {
        candidates: crate::diagnostics::health::select_try_items(
            report,
            &report.try_items,
            crate::diagnostics::health::AdviceSurface::Cosh,
            3,
        )
        .into_iter()
        .cloned()
        .collect(),
        guide: Some((store, runtime, snapshot)),
    }
}

pub(super) fn claim_displayed_health(
    prepared: &PreparedHealth,
    report: &HealthScanReport,
    displayed_ids: &[String],
) -> Vec<String> {
    if !prepared.valid() || displayed_ids.is_empty() {
        return Vec::new();
    }
    let Some((_, runtime, snapshot)) = &prepared.guide else {
        return Vec::new();
    };
    let candidates = displayed_ids
        .iter()
        .filter_map(|id| {
            let item = prepared.candidates.iter().find(|item| &item.id == id)?;
            let finding = report
                .findings
                .iter()
                .find(|finding| finding.id == item.finding_id)?;
            Some(crate::diagnostics::health::suppression::claim::Candidate {
                id: item.id.clone(),
                finding: finding.clone(),
            })
        })
        .collect::<Vec<_>>();
    crate::diagnostics::health::suppression::claim::claim(
        report,
        candidates,
        &snapshot.generation,
        runtime.is_some(),
    )
    .unwrap_or_default()
}

pub(super) fn eligible_candidates(
    prepared: &PreparedHealth,
    report: &HealthScanReport,
) -> Vec<crate::diagnostics::health::HealthTryItem> {
    let suppression =
        crate::diagnostics::health::suppression::HealthSuppressionStore::load_default();
    let host = crate::diagnostics::health::suppression::host_id_for_report(
        report,
        crate::diagnostics::health::suppression::health_machine_id().as_deref(),
    );
    let metrics = crate::diagnostics::health::suppression::metrics_for_report(report);
    let now = crate::diagnostics::health::suppression::current_time_ms();
    prepared
        .candidates
        .iter()
        .filter(|item| {
            report
                .findings
                .iter()
                .find(|finding| finding.id == item.finding_id)
                .is_some_and(|finding| {
                    suppression.should_show(&host, finding, &metrics, now)
                        == crate::diagnostics::health::suppression::SuppressionDecision::Show
                })
        })
        .cloned()
        .collect()
}

pub(super) fn write_session_health_facts<W: std::io::Write>(
    report: &HealthScanReport,
    renderer: &crate::ui::RatatuiInlineRenderer,
    output: &mut W,
) -> std::io::Result<()> {
    let facts = crate::ui::guidance::render_health(
        renderer,
        report,
        true,
        crate::ui::guidance::Target::Cosh,
        None,
    )
    .unwrap_or_default();
    output.write_all(facts.as_bytes())?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::health::suppression::metrics_for_report;
    use crate::diagnostics::health::suppression::HealthSuppressionStore;
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

    fn eligible_ids(
        report: &HealthScanReport,
        suppression: &HealthSuppressionStore,
        host: &str,
        now_ms: u64,
    ) -> Vec<String> {
        let metrics = metrics_for_report(report);
        report
            .try_items
            .iter()
            .filter(|item| {
                report
                    .findings
                    .iter()
                    .find(|finding| finding.id == item.finding_id)
                    .is_some_and(|finding| {
                        suppression.should_show(host, finding, &metrics, now_ms)
                            == crate::diagnostics::health::suppression::SuppressionDecision::Show
                    })
            })
            .map(|item| item.id.clone())
            .collect()
    }

    #[test]
    fn preparation_keeps_the_worker_pool_without_regenerating_or_mutating_it() {
        let _lock = crate::diagnostics::test_env::env_guard();
        let home = tempfile::tempdir().unwrap();
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let old = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path().canonicalize().unwrap());
        let report = report(HealthSeverity::Warning);
        let original = report.clone();
        let prepared = prepare_session(&report, Language::EnUs, true);
        match old {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        assert_eq!(prepared.candidates.len(), 1);
        assert_eq!(prepared.candidates[0].id, "worker-memory");
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
            assert!(eligible_ids(&report, &suppression, "host", 1001).is_empty());
            assert_eq!(
                eligible_ids(&report, &suppression, "host", 1000 + ttl),
                vec!["worker-memory".to_string()]
            );
        }
    }

    #[test]
    fn facts_render_once_without_questions_and_claim_respects_guide_off() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = crate::config::guide::GuideStore::open(root.join("guide")).unwrap();
        let snapshot = store.snapshot(None).unwrap();
        let report = report(HealthSeverity::Warning);
        let original = report.clone();
        let prepared = PreparedHealth {
            candidates: report.try_items.clone(),
            guide: Some((store, None, snapshot)),
        };
        let mut output = Vec::new();
        write_session_health_facts(
            &report,
            &crate::ui::RatatuiInlineRenderer::plain_with_width(80),
            &mut output,
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("Health check"));
        assert!(!text.contains("Ask directly"));
        assert!(!text.contains("cosh --ask"));
        assert_eq!(text.matches("Health check").count(), 1);
        prepared
            .guide
            .as_ref()
            .unwrap()
            .0
            .set_guide(None, false)
            .unwrap();
        assert!(
            claim_displayed_health(&prepared, &report, &["worker-memory".to_string()]).is_empty()
        );
        assert_eq!(report, original);
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
        assert_eq!(
            eligible_ids(&report, &suppression, "host", 1001),
            vec!["worker-swap".to_string()]
        );
        assert_eq!(report, original);
    }
}
