use super::*;
use crate::diagnostics::health::HealthSeverity;
use std::os::unix::fs::PermissionsExt;

struct Fixture {
    _directory: tempfile::TempDir,
    store: GuideStore,
    runtime: RuntimeState,
    snapshot: GuideSnapshot,
}

impl Fixture {
    fn new() -> Self {
        #[cfg(target_os = "linux")]
        let directory = tempfile::tempdir_in("/dev/shm").unwrap();
        #[cfg(not(target_os = "linux"))]
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = GuideStore::open(root.join("persistent")).unwrap();
        let runtime = RuntimeState::open(&root).unwrap();
        store.initialize_runtime(&runtime).unwrap();
        let snapshot = store.snapshot(Some(&runtime)).unwrap();
        Self {
            _directory: directory,
            store,
            runtime,
            snapshot,
        }
    }

    fn claim(
        &self,
        session: &str,
        now_ms: u64,
        evidence: &str,
        severity: HealthSeverity,
        legacy_problems: &[String],
    ) -> ClaimDecision {
        self.store
            .claim_health_version(
                Some(&self.runtime),
                &self.snapshot,
                ClaimRequest {
                    kind: InviteKind::LoginHealth,
                    host: "host",
                    problem: "T03",
                    session,
                },
                now_ms,
                HealthExposure {
                    evidence,
                    event: Some(("fixture-v1", evidence)),
                    severity: Some(severity.precedence()),
                    legacy_problems,
                },
            )
            .unwrap()
    }
}

#[test]
fn same_oom_event_aging_from_critical_to_warning_is_not_a_new_exposure() {
    let fixture = Fixture::new();
    assert!(matches!(
        fixture.claim("critical", 1000, "event-1", HealthSeverity::Critical, &[]),
        ClaimDecision::Claimed(_)
    ));
    assert!(matches!(
        fixture.claim("warning", 301_000, "event-1", HealthSeverity::Warning, &[]),
        ClaimDecision::Suppressed
    ));
    assert!(matches!(
        fixture.claim(
            "critical-again",
            302_000,
            "event-1",
            HealthSeverity::Critical,
            &[]
        ),
        ClaimDecision::Suppressed
    ));
}

#[test]
fn unchanged_event_severity_upgrade_is_exposed_once() {
    let fixture = Fixture::new();
    assert!(matches!(
        fixture.claim("warning", 1000, "event-1", HealthSeverity::Warning, &[]),
        ClaimDecision::Claimed(_)
    ));
    assert!(matches!(
        fixture.claim("critical", 2000, "event-1", HealthSeverity::Critical, &[]),
        ClaimDecision::Claimed(_)
    ));
    assert!(matches!(
        fixture.claim("repeat", 3000, "event-1", HealthSeverity::Critical, &[]),
        ClaimDecision::Suppressed
    ));
    assert!(matches!(
        fixture.claim(
            "downgrade",
            DAY_MS + 1500,
            "event-1",
            HealthSeverity::Warning,
            &[]
        ),
        ClaimDecision::Suppressed
    ));
}

#[test]
fn new_event_or_object_can_be_exposed_within_the_same_help_window() {
    let fixture = Fixture::new();
    for (index, evidence) in ["event-1:/group/a", "event-2:/group/a", "event-2:/group/b"]
        .into_iter()
        .enumerate()
    {
        assert!(matches!(
            fixture.claim(
                &format!("new-{index}"),
                1000 + index as u64 * 1000,
                evidence,
                HealthSeverity::Warning,
                &[]
            ),
            ClaimDecision::Claimed(_)
        ));
        assert!(matches!(
            fixture.claim(
                &format!("repeat-{index}"),
                1500 + index as u64 * 1000,
                evidence,
                HealthSeverity::Warning,
                &[]
            ),
            ClaimDecision::Suppressed
        ));
    }
}

#[test]
fn legacy_v1_rule_identity_keeps_its_original_suppression_window() {
    let fixture = Fixture::new();
    let legacy = fixture
        .store
        .claim(
            Some(&fixture.runtime),
            &fixture.snapshot,
            ClaimRequest {
                kind: InviteKind::LoginHealth,
                host: "host",
                problem: "T03:J11",
                session: "legacy",
            },
            1000,
        )
        .unwrap();
    assert!(matches!(legacy, ClaimDecision::Claimed(_)));
    let aliases = vec!["T03:J11".into(), "T03:J12".into(), "T03:J13".into()];
    assert!(matches!(
        fixture.claim(
            "upgraded",
            2000,
            "new-event",
            HealthSeverity::Critical,
            &aliases
        ),
        ClaimDecision::Suppressed
    ));
    assert!(matches!(
        fixture.claim(
            "expired",
            DAY_MS + 1001,
            "new-event",
            HealthSeverity::Critical,
            &aliases
        ),
        ClaimDecision::Claimed(_)
    ));
}

#[test]
fn new_health_claim_suppresses_a_v1_reader_without_blocking_new_events() {
    let fixture = Fixture::new();
    let aliases = vec!["T03:J11".into(), "T03:J12".into(), "T03:J13".into()];
    for (index, evidence) in ["event-1", "event-2"].into_iter().enumerate() {
        let now = 1000 + index as u64 * 1000;
        assert!(matches!(
            fixture.claim(
                &format!("new-{index}"),
                now,
                evidence,
                HealthSeverity::Critical,
                &aliases
            ),
            ClaimDecision::Claimed(_)
        ));
        let legacy = fixture
            .store
            .claim(
                Some(&fixture.runtime),
                &fixture.snapshot,
                ClaimRequest {
                    kind: InviteKind::LoginHealth,
                    host: "host",
                    problem: "T03:J11",
                    session: "old-reader",
                },
                now + 1,
            )
            .unwrap();
        assert!(matches!(legacy, ClaimDecision::Suppressed), "{legacy:?}");
        assert!(matches!(
            fixture.claim(
                "repeat",
                now + 2,
                evidence,
                HealthSeverity::Critical,
                &aliases
            ),
            ClaimDecision::Suppressed
        ));
    }
    assert_eq!(fixture.store.read_claims().unwrap().entries.len(), 2);
}

#[test]
fn actual_bash_candidates_keep_the_same_help_and_event_when_oom_ages() {
    use crate::diagnostics::health::{
        HealthFactCategory, HealthFactSource, HealthFactValue, HealthFinding,
        HealthFindingCategory, HealthMessageId, HealthReportBuilder, HealthTryItem, HealthTryKind,
    };
    use std::collections::BTreeMap;

    let fixture = Fixture::new();
    for (index, (rule, severity)) in [
        ("J11", HealthSeverity::Critical),
        ("J12", HealthSeverity::Warning),
    ]
    .into_iter()
    .enumerate()
    {
        let mut builder = HealthReportBuilder::for_started_at(1000 + index as u128);
        builder.add_fact(
            HealthFactCategory::Kernel,
            "kernel.oom_latest_event_identity",
            HealthFactValue::String("epoch:990".into()),
            None,
            HealthFactSource::Fixture,
            0,
        );
        let mut report = builder.finish(1000 + index as u128);
        report.findings.push(HealthFinding {
            id: rule.into(),
            severity,
            category: HealthFindingCategory::RootCause,
            title_id: HealthMessageId::HealthFindingRecentOom,
            detail_id: None,
            detail_args: BTreeMap::new(),
            evidence_fact_ids: report.facts.iter().map(|fact| fact.id.clone()).collect(),
            suggested_try_ids: vec![],
        });
        report.try_items.push(HealthTryItem {
            id: format!("T03:{rule}"),
            finding_id: rule.into(),
            label_id: HealthMessageId::HealthTryCheckRecentOom,
            label_args: BTreeMap::new(),
            prompt_id: Some(HealthMessageId::HealthPromptCheckRecentOom),
            prompt_args: BTreeMap::new(),
            kind: HealthTryKind::AskAgent,
            command: None,
            score: 100,
            reason_id: HealthMessageId::HealthTryReasonRecentOom,
            reason_args: BTreeMap::new(),
        });
        let candidate =
            crate::ui::guidance::health_candidate(&report, crate::Language::EnUs).unwrap();
        assert_eq!(candidate.problem, "T03");
        assert_eq!(
            candidate.legacy_problems.first(),
            Some(&format!("T03:{rule}"))
        );
        assert!(candidate.legacy_problems.contains(&"T03:J11".into()));
        let decision = fixture
            .store
            .claim_health_version(
                Some(&fixture.runtime),
                &fixture.snapshot,
                ClaimRequest {
                    kind: InviteKind::LoginHealth,
                    host: "host",
                    problem: &candidate.problem,
                    session: &report.scan_id,
                },
                1000 + index as u64 * 300_000,
                HealthExposure {
                    evidence: candidate.evidence.as_deref().unwrap(),
                    event: candidate.event.as_ref().map(|(domain, value)| (*domain, value.as_str())),
                    severity: candidate.severity,
                    legacy_problems: &candidate.legacy_problems,
                },
            )
            .unwrap();
        if index == 0 {
            assert!(matches!(decision, ClaimDecision::Claimed(_)));
        } else {
            assert!(matches!(decision, ClaimDecision::Suppressed));
        }
    }
}
