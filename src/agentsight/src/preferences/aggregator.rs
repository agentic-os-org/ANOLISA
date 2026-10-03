//! Aggregation: folds a window's [`PreferenceSignal`]s into [`Preference`]s.
//!
//! Signals sharing the same (category, key, value) tuple merge into one
//! entry whose confidence grows with evidence count; mutually exclusive
//! keys with two strongly evidenced values are flagged as conflicting.

use std::collections::BTreeMap;

use super::signals::{
    Preference, PreferenceCategory, PreferenceSignal, PreferenceSource, PreferenceStatus,
    is_exclusive_key,
};

/// Per-observation decay base: confidence = 1 - CONFIDENCE_DECAY^n.
pub const CONFIDENCE_DECAY: f64 = 0.8;
/// Confidence is capped below 1.0 — rule evidence is never absolute proof.
pub const MAX_CONFIDENCE: f64 = 0.99;
/// Both values of an exclusive key need at least this much evidence before
/// the pair is marked conflicting (fewer observations are just noise).
pub const CONFLICT_MIN_EVIDENCE: usize = 3;
/// Max deduplicated evidence excerpts kept per aggregated preference.
pub const MAX_SAMPLE_EVIDENCE: usize = 3;

/// Saturating confidence curve: 1 observation ≈ 0.2, 3 ≈ 0.49, 10 ≈ 0.89,
/// capped at [`MAX_CONFIDENCE`].
pub fn confidence_for_evidence(count: usize) -> f64 {
    if count == 0 {
        return 0.0;
    }
    let raw = 1.0 - CONFIDENCE_DECAY.powi(count.min(i32::MAX as usize) as i32);
    raw.min(MAX_CONFIDENCE)
}

/// Fold raw signals into aggregated preferences (source is always `rule`;
/// the LLM layer produces its own entries).
///
/// Output is deterministically ordered by (category, key, value) and every
/// entry starts `active`; exclusive keys where two values each collected at
/// least [`CONFLICT_MIN_EVIDENCE`] observations have *both* sides flipped
/// to `conflicting`.
pub fn aggregate(signals: &[PreferenceSignal]) -> Vec<Preference> {
    // BTreeMap gives the deterministic ordering for free.
    let mut groups: BTreeMap<(PreferenceCategory, &'static str, String), Group> = BTreeMap::new();
    for signal in signals {
        let tuple = (
            signal.kind.category(),
            signal.kind.key(),
            signal.kind.value(),
        );
        let group = groups.entry(tuple).or_default();
        group.count += 1;
        if group.samples.len() < MAX_SAMPLE_EVIDENCE
            && !signal.excerpt.is_empty()
            && !group.samples.iter().any(|s| s == &signal.excerpt)
        {
            group.samples.push(signal.excerpt.clone());
        }
    }

    let mut prefs: Vec<Preference> = groups
        .into_iter()
        .map(|((category, key, value), group)| Preference {
            category,
            key: key.to_string(),
            value,
            confidence: confidence_for_evidence(group.count),
            evidence_count: group.count,
            source: PreferenceSource::Rule,
            status: PreferenceStatus::Active,
            sample_evidence: group.samples,
        })
        .collect();

    mark_conflicts(&mut prefs);
    prefs
}

#[derive(Default)]
struct Group {
    count: usize,
    samples: Vec<String>,
}

/// Flip both sides of an exclusive key to `conflicting` when at least two
/// of its values are strongly evidenced. Public so the API layer can re-run
/// conflict marking after merging LLM findings into the rule results — the
/// exclusivity semantics must hold across sources, not per source.
pub fn mark_conflicts(prefs: &mut [Preference]) {
    let mut strong_values: BTreeMap<(PreferenceCategory, &str), usize> = BTreeMap::new();
    for pref in prefs.iter() {
        if is_exclusive_key(&pref.key) && is_strong(pref) {
            *strong_values
                .entry((pref.category, pref.key.as_str()))
                .or_default() += 1;
        }
    }
    let conflicted: Vec<(PreferenceCategory, String)> = strong_values
        .into_iter()
        .filter(|(_, n)| *n >= 2)
        .map(|((category, key), _)| (category, key.to_string()))
        .collect();
    for pref in prefs.iter_mut() {
        if is_strong(pref)
            && conflicted
                .iter()
                .any(|(c, k)| *c == pref.category && *k == pref.key)
        {
            pref.status = PreferenceStatus::Conflicting;
        }
    }
}

/// Whether an entry is weighty enough to participate in conflict detection:
/// [`CONFLICT_MIN_EVIDENCE`]+ rule observations, or any LLM finding — one
/// LLM row is already a holistic judgment over the whole window, not a
/// single observation.
fn is_strong(pref: &Preference) -> bool {
    pref.source == PreferenceSource::Llm || pref.evidence_count >= CONFLICT_MIN_EVIDENCE
}

#[cfg(test)]
mod tests {
    use super::super::signals::SignalKind;
    use super::*;

    fn signal(kind: SignalKind, event_id: i64, excerpt: &str) -> PreferenceSignal {
        PreferenceSignal {
            kind,
            event_id,
            session_id: Some("s1".to_string()),
            timestamp_ns: Some(event_id),
            excerpt: excerpt.to_string(),
        }
    }

    #[test]
    fn empty_input_yields_no_preferences() {
        assert!(aggregate(&[]).is_empty());
    }

    #[test]
    fn confidence_curve_saturates_below_one() {
        assert_eq!(confidence_for_evidence(0), 0.0);
        assert!((confidence_for_evidence(1) - 0.2).abs() < 1e-9);
        assert!((confidence_for_evidence(3) - 0.488).abs() < 1e-3);
        assert!(confidence_for_evidence(10) > 0.85);
        assert!(confidence_for_evidence(10_000) <= MAX_CONFIDENCE);
    }

    #[test]
    fn merges_same_tuple_and_dedups_samples() {
        let signals = vec![
            signal(SignalKind::TestRequirement, 1, "跑测试"),
            signal(SignalKind::TestRequirement, 2, "run tests please"),
            signal(SignalKind::TestRequirement, 3, "跑测试"), // duplicate excerpt
            signal(SignalKind::PlanFirst, 4, "先出方案"),
        ];
        let prefs = aggregate(&signals);
        assert_eq!(prefs.len(), 2);

        let testing = prefs.iter().find(|p| p.key == "testing").expect("testing");
        assert_eq!(testing.evidence_count, 3);
        assert_eq!(testing.value, "require_tests");
        assert_eq!(testing.source, PreferenceSource::Rule);
        assert_eq!(testing.status, PreferenceStatus::Active);
        assert_eq!(testing.sample_evidence, vec!["跑测试", "run tests please"]);

        let plan = prefs
            .iter()
            .find(|p| p.key == "workflow_style")
            .expect("plan");
        assert_eq!(plan.evidence_count, 1);
        assert!((plan.confidence - 0.2).abs() < 1e-9);
    }

    #[test]
    fn exclusive_key_with_two_strong_values_conflicts() {
        let mut signals = Vec::new();
        for i in 0..3 {
            signals.push(signal(
                SignalKind::Language {
                    language: "chinese",
                },
                i,
                "中文查询",
            ));
            signals.push(signal(
                SignalKind::Language {
                    language: "english",
                },
                100 + i,
                "english query",
            ));
        }
        let prefs = aggregate(&signals);
        assert_eq!(prefs.len(), 2);
        assert!(
            prefs
                .iter()
                .all(|p| p.status == PreferenceStatus::Conflicting)
        );
    }

    #[test]
    fn weak_opposition_or_non_exclusive_key_stays_active() {
        // Only 2 english observations: below CONFLICT_MIN_EVIDENCE.
        let mut signals = Vec::new();
        for i in 0..3 {
            signals.push(signal(
                SignalKind::Language {
                    language: "chinese",
                },
                i,
                "中文查询",
            ));
        }
        for i in 0..2 {
            signals.push(signal(
                SignalKind::Language {
                    language: "english",
                },
                100 + i,
                "english query",
            ));
        }
        let prefs = aggregate(&signals);
        assert!(prefs.iter().all(|p| p.status == PreferenceStatus::Active));

        // preferred_tool is not exclusive: many strong values coexist.
        let mut signals = Vec::new();
        for i in 0..4 {
            signals.push(signal(
                SignalKind::ToolUsage {
                    tool_name: "bash".to_string(),
                },
                i,
                "tool_call: bash",
            ));
            signals.push(signal(
                SignalKind::ToolUsage {
                    tool_name: "grep".to_string(),
                },
                100 + i,
                "tool_call: grep",
            ));
        }
        let prefs = aggregate(&signals);
        assert_eq!(prefs.len(), 2);
        assert!(prefs.iter().all(|p| p.status == PreferenceStatus::Active));
    }
}
