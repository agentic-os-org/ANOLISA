//! Per-request custom matching with independent completeness accounting.

use crate::models::{
    Candidate, CustomRuleStatus, CustomRuleSummary, ScanError, Span, check_deadline,
};
use crate::rules::PiiRuleSet;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

const MAX_FINDINGS: usize = 100;
const SCAN_BUDGET: Duration = Duration::from_millis(200);

pub(crate) struct CustomRun<'a> {
    pub candidates: Vec<Candidate<'a>>,
    pub summary: CustomRuleSummary,
    pub reasons: BTreeSet<&'static str>,
}

pub(crate) fn detect<'a>(
    input: &'a str,
    rules: &'a PiiRuleSet,
    deadline: Option<Instant>,
) -> Result<CustomRun<'a>, ScanError> {
    run(input, rules, Instant::now() + SCAN_BUDGET, deadline)
}

fn run<'a>(
    input: &'a str,
    rules: &'a PiiRuleSet,
    deadline: Instant,
    scan_deadline: Option<Instant>,
) -> Result<CustomRun<'a>, ScanError> {
    check_deadline(scan_deadline)?;
    let mut result = CustomRun {
        candidates: Vec::new(),
        summary: rules.summary.clone(),
        reasons: BTreeSet::new(),
    };
    if result.summary.status == CustomRuleStatus::Invalid {
        result.reasons.insert("custom_rules_invalid");
    }
    if rules.custom.is_empty() {
        return Ok(result);
    }
    let offsets: Vec<_> = input
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(input.len()))
        .collect();
    'rules: for rule in &rules.custom {
        let mut cursor = rule.pattern.find_iter(input);
        loop {
            check_deadline(scan_deadline)?;
            if Instant::now() >= deadline {
                result.summary.budget_exhausted = true;
                result.reasons.insert("custom_budget_exhausted");
                break 'rules;
            }
            // The loop budget is cooperative between matches, not a promise to
            // interrupt one regex call. Each call also has a fixed backtracking limit.
            let next = cursor.next();
            check_deadline(scan_deadline)?;
            if let Some(Err(_)) = &next {
                result.summary.runtime_error_count += 1;
                result.reasons.insert("custom_matching_limited");
            }
            if Instant::now() >= deadline {
                result.summary.budget_exhausted = true;
                result.reasons.insert("custom_budget_exhausted");
                break 'rules;
            }
            let matched = match next {
                None | Some(Err(_)) => break,
                Some(Ok(matched)) => matched,
            };
            if matched.start() == matched.end() {
                result.summary.runtime_error_count += 1;
                result.reasons.insert("custom_empty_match");
                continue;
            }
            if result.candidates.len() == MAX_FINDINGS {
                result.summary.truncated = true;
                result.reasons.insert("custom_findings_limited");
                break 'rules;
            }
            result.candidates.push(Candidate {
                kind: &rule.kind,
                category: "custom",
                severity: rule.severity,
                confidence: 1.0,
                value: matched.as_str(),
                span: Span {
                    start: offsets.partition_point(|i| *i < matched.start()),
                    end: offsets.partition_point(|i| *i < matched.end()),
                },
                bytes: matched.range(),
                metadata: BTreeMap::from([
                    ("detector".into(), json!("custom_rule")),
                    ("engine".into(), json!("fancy_regex")),
                ]),
            });
        }
    }
    result.candidates.sort_by(Candidate::compare);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_loop_budget_is_reported_without_starting_a_rule() {
        let rules = PiiRuleSet::from_yaml(b"- type: marker\n  regex: TOKEN\n").unwrap();
        let deadline = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .unwrap();
        let result = run("TOKEN", &rules, deadline, None).unwrap();
        assert!(result.candidates.is_empty());
        assert!(result.summary.budget_exhausted);
        assert!(result.reasons.contains("custom_budget_exhausted"));
        assert!(!rules.summary.budget_exhausted);
    }
}
