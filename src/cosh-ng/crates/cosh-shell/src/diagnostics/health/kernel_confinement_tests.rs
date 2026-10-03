//! Tests for the #3413 kernel-confinement facts and judgement rule.

use super::*;
use crate::config::{HealthConfig, Language};
use crate::diagnostics::health::{
    apply_judgement_rules, finding_remediation, sorted_findings, HealthFact, HealthMessageId,
    HealthScanReport, HealthSeverity,
};
use crate::I18n;

use super::*;

#[test]
fn kernel_release_decides_openat2_support() {
    // Reported by #3413: RHEL/Anolis 8 style kernels predate openat2(2).
    assert_eq!(
        kernel_supports_openat2("4.19.112-2.el8.x86_64"),
        Some(false)
    );
    assert_eq!(
        kernel_supports_openat2("4.18.0-553.el8_10.x86_64"),
        Some(false)
    );
    assert_eq!(
        kernel_supports_openat2("3.10.0-1160.el7.x86_64"),
        Some(false)
    );
    assert_eq!(
        kernel_supports_openat2("5.5.9-200.fc31.x86_64"),
        Some(false)
    );
    // 5.6 is the first release with openat2(2).
    assert_eq!(kernel_supports_openat2("5.6.0"), Some(true));
    assert_eq!(kernel_supports_openat2("5.6.19-hardened"), Some(true));
    assert_eq!(
        kernel_supports_openat2("5.10.134-16.al8.x86_64"),
        Some(true)
    );
    assert_eq!(kernel_supports_openat2("6.6.0-rc1-generic"), Some(true));
    assert_eq!(kernel_supports_openat2("6"), Some(true));
}

#[test]
fn unparsable_kernel_release_stays_unknown() {
    for release in ["", "   ", "garbage", "unknown", ".5.6", "-1.2"] {
        assert_eq!(parse_kernel_version(release), None, "{release}");
        assert_eq!(kernel_supports_openat2(release), None, "{release}");
    }
}

#[test]
fn parse_kernel_version_reads_major_and_minor_only() {
    assert_eq!(parse_kernel_version("4.19.112-2.el8.x86_64"), Some((4, 19)));
    assert_eq!(
        parse_kernel_version("5.10.134-16.al8.x86_64"),
        Some((5, 10))
    );
    assert_eq!(parse_kernel_version("6.6"), Some((6, 6)));
    assert_eq!(parse_kernel_version("6"), Some((6, 0)));
}

#[test]
fn confinement_facts_follow_the_parsed_release() {
    let report_for = |release: &str| {
        let mut builder = HealthReportBuilder::for_started_at(1);
        record_release_facts(&mut builder, release, 0);
        builder.finish(1)
    };

    let unsupported = report_for("4.19.112-2.el8.x86_64");
    assert!(unsupported
        .facts
        .iter()
        .any(|fact| fact.key == "kernel.release"
            && fact.value == HealthFactValue::String("4.19.112-2.el8.x86_64".to_string())
            && fact.source == HealthFactSource::ProcSysKernel));
    assert!(unsupported
        .facts
        .iter()
        .any(|fact| fact.key == "kernel.openat2_supported"
            && fact.value == HealthFactValue::Bool(false)));

    let supported = report_for("5.10.134-16.al8.x86_64");
    assert!(supported
        .facts
        .iter()
        .any(|fact| fact.key == "kernel.openat2_supported"
            && fact.value == HealthFactValue::Bool(true)));

    let unknown = report_for("garbage");
    assert!(unknown
        .facts
        .iter()
        .any(|fact| fact.key == "kernel.release"));
    assert!(!unknown
        .facts
        .iter()
        .any(|fact| fact.key == "kernel.openat2_supported"));
}

// ─── Judgement rule J17 ───────────────────────────────────────────────────

fn confinement_report(facts: Vec<HealthFact>) -> HealthScanReport {
    let mut report = HealthScanReport::new("health-confinement", 0);
    report.facts = facts;
    apply_judgement_rules(&mut report, &HealthConfig::default());
    report
}

fn openat2_fact(supported: bool) -> HealthFact {
    HealthFact {
        id: "kernel.openat2_supported".to_string(),
        category: HealthFactCategory::Kernel,
        key: "kernel.openat2_supported".to_string(),
        value: HealthFactValue::Bool(supported),
        unit: None,
        source: HealthFactSource::Derived,
        elapsed_ms: 0,
    }
}

fn release_fact(release: &str) -> HealthFact {
    HealthFact {
        id: "kernel.release".to_string(),
        category: HealthFactCategory::Kernel,
        key: "kernel.release".to_string(),
        value: HealthFactValue::String(release.to_string()),
        unit: None,
        source: HealthFactSource::ProcSysKernel,
        elapsed_ms: 0,
    }
}

fn finding_ids(report: &HealthScanReport) -> Vec<String> {
    report
        .findings
        .iter()
        .map(|finding| finding.id.clone())
        .collect()
}

fn oom_age_fact(seconds: f64) -> HealthFact {
    HealthFact {
        id: "kernel.oom_latest_age_seconds".to_string(),
        category: HealthFactCategory::Kernel,
        key: "kernel.oom_latest_age_seconds".to_string(),
        value: HealthFactValue::Float(seconds),
        unit: None,
        source: HealthFactSource::Fixture,
        elapsed_ms: 0,
    }
}

#[test]
fn missing_openat2_is_a_critical_finding_carrying_kernel_context() {
    let report = confinement_report(vec![
        openat2_fact(false),
        release_fact("4.19.112-2.el8.x86_64"),
    ]);

    let confinement = report
        .findings
        .iter()
        .find(|finding| finding.id == "J17")
        .expect("J17 finding");
    assert_eq!(confinement.severity, HealthSeverity::Critical);
    assert_eq!(report.overall_severity, HealthSeverity::Critical);
    assert_eq!(
        confinement.title_id,
        HealthMessageId::HealthFindingWorkspaceConfinementUnsupported
    );
    assert_eq!(
        confinement.detail_id,
        Some(HealthMessageId::HealthRemediationWorkspaceConfinement)
    );
    assert_eq!(
        confinement.detail_args.get("kernel").map(String::as_str),
        Some("4.19.112-2.el8.x86_64")
    );
    assert_eq!(
        confinement.detail_args.get("required").map(String::as_str),
        Some("5.6")
    );
    assert_eq!(
        confinement.evidence_fact_ids,
        vec![
            "kernel.openat2_supported".to_string(),
            "kernel.release".to_string()
        ]
    );
}

#[test]
fn supported_or_unknown_kernel_adds_no_confinement_finding() {
    let supported = confinement_report(vec![
        openat2_fact(true),
        release_fact("5.10.134-16.al8.x86_64"),
    ]);
    assert_eq!(finding_ids(&supported), Vec::<String>::new());

    // An unparsable release records no support fact; that must stay silent
    // instead of guessing "unsupported".
    let unknown = confinement_report(vec![release_fact("garbage")]);
    assert_eq!(finding_ids(&unknown), Vec::<String>::new());
}

#[test]
fn confinement_finding_leads_the_visible_order() {
    let report = confinement_report(vec![openat2_fact(false), oom_age_fact(120.0)]);

    // Both findings are critical; the visible order must still lead with the
    // one that stops the agent from running at all.
    assert!(finding_ids(&report).contains(&"J11".to_string()));
    let visible = sorted_findings(&report);
    assert_eq!(visible[0].id, "J17");
    assert_eq!(visible[1].id, "J11");
}

#[test]
fn confinement_messages_render_kernel_and_requirement_in_both_languages() {
    let report = confinement_report(vec![
        openat2_fact(false),
        release_fact("4.19.112-2.el8.x86_64"),
    ]);
    let finding = report
        .findings
        .iter()
        .find(|finding| finding.id == "J17")
        .expect("J17 finding");

    for language in [Language::EnUs, Language::ZhCn] {
        let i18n = I18n::new(language);
        let args: Vec<(&str, &str)> = finding
            .detail_args
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let title = i18n.format(finding.title_id.to_i18n(), &args);
        let remediation = finding_remediation(finding, i18n).expect("remediation");

        assert!(title.contains("5.6"), "{title}");
        assert!(
            remediation.contains("4.19.112-2.el8.x86_64"),
            "{remediation}"
        );
        assert!(remediation.contains("5.6"), "{remediation}");
        for text in [&title, &remediation] {
            assert!(!text.contains('{'), "unrendered placeholder: {text}");
            assert!(!text.trim().is_empty(), "{text}");
        }
    }
}
