//! Strategy orchestrator — runs all strategies in parallel, merges/deduplicates,
//! sorts by evidence tier, and applies rule-derived gates to produce the final
//! `Vec<AccIssue>`.

use std::collections::HashMap;
use std::path::Path;

use futures::future::join_all;

use crate::llm::LlmClient;
use crate::trace::TraceInventory;
use crate::types::{AccIssue, AccRootCause, FixLocus, RootObject};

use crate::accuracy::detector::{AnalysisCtx, Detector, RawIssue};
use crate::accuracy::extract::SharedExtraction;
use crate::accuracy::strategies::confirm_before_act::ConfirmBeforeActStrategy;
use crate::accuracy::strategies::experience_library::ExperienceLibraryStrategy;
use crate::accuracy::strategies::fact_check::FactCheckStrategy;
use crate::accuracy::strategies::requirement_check::RequirementCheckStrategy;
use crate::accuracy::strategies::verify_before_done::VerifyBeforeDoneStrategy;

/// Run all strategies in parallel, merge, deduplicate, sort, and apply gates.
pub async fn run_strategies(
    client: &LlmClient,
    inv: &TraceInventory,
    extraction: &SharedExtraction,
    repo_root: Option<&Path>,
) -> Vec<AccIssue> {
    let ctx = AnalysisCtx {
        inv,
        client,
        repo_root,
        extraction,
    };

    // Build strategy list.
    let detectors: Vec<Box<dyn Detector>> = vec![
        Box::new(VerifyBeforeDoneStrategy::new()),
        Box::new(RequirementCheckStrategy::new()),
        Box::new(ConfirmBeforeActStrategy::new()),
        Box::new(FactCheckStrategy::new()),
        Box::new(ExperienceLibraryStrategy::new()),
    ];

    tracing::info!(
        "[accuracy] Running {} strategies in parallel...",
        detectors.len()
    );

    // Run all detectors concurrently.
    let futures: Vec<_> = detectors
        .iter()
        .map(|d| {
            let name = d.name();
            let fut = d.detect(&ctx);
            tracing::info!("[accuracy] Detector '{}' — running...", name);
            async move {
                let issues = fut.await;
                if issues.is_empty() {
                    tracing::info!("[accuracy] Detector '{}' ✗ no issues", name);
                } else {
                    tracing::info!(
                        "[accuracy] Detector '{}' ✓ produced {} issues",
                        name,
                        issues.len()
                    );
                }
                (name, issues)
            }
        })
        .collect();

    let results = join_all(futures).await;

    // Flatten all raw issues.
    let all_raw: Vec<(/*detector*/ &str, RawIssue)> = results
        .into_iter()
        .flat_map(|(name, issues)| issues.into_iter().map(move |i| (name, i)))
        .collect();

    tracing::info!(
        "[accuracy] Total raw issues before dedup: {}",
        all_raw.len()
    );

    // Deduplicate: same (defect_type, tool_call_id anchor) → keep strongest tier.
    let deduped = deduplicate(all_raw);

    // Apply rule-derived gates, then sort: strongest evidence first (L1 → L5),
    // same tier ordered by symptom for stable output.
    let mut issues: Vec<AccIssue> = deduped
        .into_iter()
        .map(|(_, raw)| raw_to_acc_issue(raw, inv))
        .collect();
    issues.sort_by(|a, b| {
        a.evidence_tier
            .cmp(&b.evidence_tier)
            .then_with(|| a.symptom.cmp(&b.symptom))
    });
    issues
}

/// Deduplicate issues: same (defect_type, tool_call_id) keeps the strongest tier.
fn deduplicate(raw_issues: Vec<(&str, RawIssue)>) -> Vec<(&str, RawIssue)> {
    // Key = (defect_type, tool_call_id or symptom hash).
    let mut best: HashMap<(String, String), (/*detector*/ &str, RawIssue)> = HashMap::new();

    for (detector, issue) in raw_issues {
        let key = (
            format!("{:?}", issue.defect_type),
            issue
                .tool_call_id
                .clone()
                .unwrap_or_else(|| issue.symptom.clone()),
        );

        let should_replace = match best.get(&key) {
            Some((_, existing)) => issue.evidence_tier < existing.evidence_tier,
            None => true,
        };

        if should_replace {
            best.insert(key, (detector, issue));
        }
    }

    best.into_values().collect()
}

/// Convert a `RawIssue` to a finalized `AccIssue` by applying rule-derived gates.
fn raw_to_acc_issue(raw: RawIssue, inv: &TraceInventory) -> AccIssue {
    let fix_locus = FixLocus::from_primary(&raw.primary_object);
    let confidence = raw.evidence_tier.confidence().to_string();
    let auto_patch = raw.evidence_tier.allows_auto_patch();
    let optimizable = !matches!(raw.primary_object, RootObject::Env | RootObject::Input);

    // Recovered detection: anchored err later succeeded by same (name, cmd).
    let (evidence, at, recovered) = match raw.tool_call_id.as_deref() {
        Some(id) => {
            if let Some(rec) = inv.tool_calls.iter().find(|c| c.call_id == id) {
                let ev = format!(
                    "{} {}{}",
                    rec.name,
                    rec.cmd,
                    if rec.err { " ✗" } else { "" }
                );
                let at_str = format!("@{}s", rec.start.round() as i64);
                let rec_later = rec.err && recovered_later(&inv.tool_calls, rec);
                (ev, at_str, rec_later)
            } else {
                (String::new(), String::new(), false)
            }
        }
        None => (String::new(), String::new(), false),
    };

    let tier = if recovered {
        "internal-lead"
    } else {
        "user-failure"
    };

    AccIssue {
        symptom: raw.symptom,
        defect_type: raw.defect_type,
        root_cause: vec![AccRootCause {
            object: raw.primary_object,
            role: "主因".into(),
        }],
        fix_locus,
        confidence,
        optimizable,
        tier: tier.to_string(),
        recovered,
        evidence_tier: raw.evidence_tier,
        auto_patch,
        evidence,
        at,
        detail: raw.detail,
        verify: raw.verify,
        fix: raw.fix,
    }
}

/// Whether an errored call was later followed by a successful same-(name, cmd) call.
/// Fixed: matches by (name, cmd) instead of just name to avoid false positives.
fn recovered_later(
    calls: &[crate::types::ToolCallRecord],
    errored: &crate::types::ToolCallRecord,
) -> bool {
    calls.iter().any(|c| {
        c.name == errored.name && c.cmd == errored.cmd && c.start > errored.start && !c.err
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::TraceInventory;
    use crate::types::{DefectType, EvidenceTier, ToolCallRecord};

    fn make_inv(calls: Vec<ToolCallRecord>) -> TraceInventory {
        TraceInventory {
            tool_calls: calls,
            user_turns: vec![],
            final_answer: String::new(),
            skill_contract: None,
        }
    }

    fn make_call(name: &str, cmd: &str, start: f64, err: bool) -> ToolCallRecord {
        ToolCallRecord {
            name: name.into(),
            call_id: format!("{name}_{start}"),
            start,
            dur: 1.0,
            cmd: cmd.into(),
            err,
            result_tokens: None,
        }
    }

    #[test]
    fn dedup_keeps_strongest_tier() {
        let issues = vec![
            (
                "d1",
                RawIssue {
                    symptom: "test".into(),
                    defect_type: DefectType::Workflow,
                    primary_object: RootObject::Skill,
                    evidence_tier: EvidenceTier::L4,
                    tool_call_id: Some("abc".into()),
                    detail: String::new(),
                    verify: String::new(),
                    fix: String::new(),
                },
            ),
            (
                "d2",
                RawIssue {
                    symptom: "test".into(),
                    defect_type: DefectType::Workflow,
                    primary_object: RootObject::Skill,
                    evidence_tier: EvidenceTier::L1,
                    tool_call_id: Some("abc".into()),
                    detail: String::new(),
                    verify: String::new(),
                    fix: String::new(),
                },
            ),
        ];

        let deduped = deduplicate(issues);
        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].1.evidence_tier, EvidenceTier::L1);
    }

    #[test]
    fn recovered_matches_by_name_and_cmd() {
        let calls = vec![
            make_call("Bash", "git clone url1", 10.0, true),
            make_call("Bash", "git clone url2", 20.0, false), // different cmd → NOT recovered
        ];
        assert!(!recovered_later(&calls, &calls[0]));

        let calls2 = vec![
            make_call("Bash", "git clone url1", 10.0, true),
            make_call("Bash", "git clone url1", 20.0, false), // same cmd → recovered
        ];
        assert!(recovered_later(&calls2, &calls2[0]));
    }

    #[test]
    fn confidence_derived_from_tier() {
        assert_eq!(EvidenceTier::L1.confidence(), "高");
        assert_eq!(EvidenceTier::L2.confidence(), "高");
        assert_eq!(EvidenceTier::L3.confidence(), "中");
        assert_eq!(EvidenceTier::L4.confidence(), "中");
        assert_eq!(EvidenceTier::L5.confidence(), "低");
    }

    #[test]
    fn auto_patch_gate() {
        assert!(EvidenceTier::L1.allows_auto_patch());
        assert!(EvidenceTier::L2.allows_auto_patch());
        assert!(EvidenceTier::L3.allows_auto_patch());
        assert!(!EvidenceTier::L4.allows_auto_patch());
        assert!(!EvidenceTier::L5.allows_auto_patch());
    }

    #[test]
    fn fix_locus_from_primary() {
        assert_eq!(FixLocus::from_primary(&RootObject::Skill), FixLocus::Skill);
        assert_eq!(
            FixLocus::from_primary(&RootObject::Context),
            FixLocus::ContextPolicy
        );
        assert_eq!(
            FixLocus::from_primary(&RootObject::Model),
            FixLocus::ModelRouting
        );
        assert_eq!(FixLocus::from_primary(&RootObject::Tool), FixLocus::Tool);
        assert_eq!(FixLocus::from_primary(&RootObject::Env), FixLocus::None);
        assert_eq!(FixLocus::from_primary(&RootObject::Input), FixLocus::None);
    }

    #[test]
    fn raw_to_acc_issue_sets_gates_correctly() {
        let inv = make_inv(vec![make_call("Edit", "file.rs", 5.0, true)]);

        let raw = RawIssue {
            symptom: "test issue".into(),
            defect_type: DefectType::Knowledge,
            primary_object: RootObject::Skill,
            evidence_tier: EvidenceTier::L2,
            tool_call_id: Some("Edit_5".into()),
            detail: "detail".into(),
            verify: "verify".into(),
            fix: "fix".into(),
        };

        let issue = raw_to_acc_issue(raw, &inv);
        assert_eq!(issue.defect_type, DefectType::Knowledge);
        assert_eq!(issue.fix_locus, FixLocus::Skill);
        assert_eq!(issue.confidence, "高");
        assert!(issue.auto_patch);
        assert!(issue.optimizable);
        assert!(!issue.recovered); // no later success
        assert!(issue.evidence.contains("Edit"));
    }

    #[test]
    fn l4_issue_has_auto_patch_false() {
        let inv = make_inv(vec![]);
        let raw = RawIssue {
            symptom: "semantic issue".into(),
            defect_type: DefectType::Workflow,
            primary_object: RootObject::Skill,
            evidence_tier: EvidenceTier::L4,
            tool_call_id: None,
            detail: String::new(),
            verify: String::new(),
            fix: String::new(),
        };

        let issue = raw_to_acc_issue(raw, &inv);
        assert!(!issue.auto_patch);
        assert_eq!(issue.confidence, "中"); // L4 capped at 中
    }
}
