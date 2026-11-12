//! Deterministic deductions preserve the published scoring weights and Critical gates.

use std::collections::{BTreeMap, HashSet};

use rust_decimal::{Decimal, RoundingStrategy, prelude::ToPrimitive};
use serde_json::{Value, json};
use unicode_casefold::UnicodeCaseFold;

use crate::Error;

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str, Error> {
    value[field]
        .as_str()
        .ok_or_else(|| Error::Contract(format!("scoring requires string field {field}")))
}

fn identity(value: &Value) -> Result<Vec<String>, Error> {
    ["entry", "operation", "boundary"]
        .into_iter()
        .map(|field| {
            Ok(text(value, field)?
                .case_fold()
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "))
        })
        .collect()
}

fn number(value: Decimal) -> Result<f64, Error> {
    value
        .to_f64()
        .ok_or_else(|| Error::Contract("score cannot be represented in JSON".into()))
}

fn rounded(value: Decimal) -> Result<f64, Error> {
    number(value.round_dp_with_strategy(1, RoundingStrategy::MidpointAwayFromZero))
}

fn deduction(finding: &Value) -> Result<Decimal, Error> {
    let severity = match text(finding, "severity")? {
        "Critical" => 0,
        "High" => 15,
        "Medium" => 10,
        "Low" => 5,
        value => return Err(Error::Contract(format!("unknown severity {value}"))),
    };
    let existence = match text(finding, "status")? {
        "material_risk" => Decimal::ONE,
        "unresolved" => Decimal::new(5, 1),
        "control_gap" | "accepted" | "refuted" => Decimal::ZERO,
        value => return Err(Error::Contract(format!("unknown finding status {value}"))),
    };
    let dynamic = match text(finding, "dynamic")? {
        "confirmed_impact" => Decimal::ONE,
        "attempted_violation" => Decimal::new(8, 1),
        "contained" => Decimal::new(3, 1),
        "no_proof" | "not_activated" | "unsupported" => Decimal::new(6, 1),
        value => return Err(Error::Contract(format!("unknown dynamic status {value}"))),
    };
    Ok(Decimal::from(severity) * existence * dynamic)
}

fn gap_weight(gap: &Value, rules: &Value) -> Result<Decimal, Error> {
    let rule = text(gap, "rule_id")?;
    let weight = rules[rule]["weight"]
        .as_u64()
        .filter(|weight| *weight <= 3)
        .ok_or_else(|| Error::Contract(format!("unknown hygiene rule {rule}")))?;
    let factor = match text(gap, "status")? {
        "confirmed" => Decimal::ONE,
        "uncertain" => Decimal::new(5, 1),
        "refuted" | "accepted" => Decimal::ZERO,
        value => {
            return Err(Error::Contract(format!(
                "unknown control gap status {value}"
            )));
        }
    };
    Ok(Decimal::from(weight) * factor)
}

fn score_gaps(gaps: &[Value], findings: &[Value]) -> Result<(Value, Decimal), Error> {
    let rules: Value = serde_json::from_str(include_str!("../assets/hygiene.json"))
        .map_err(|error| Error::Contract(format!("invalid embedded hygiene rules: {error}")))?;
    let mut promoted = HashSet::new();
    let mut risk_keys = HashSet::new();
    for finding in findings {
        if text(finding, "scope")? == "skill"
            && matches!(text(finding, "status")?, "material_risk" | "unresolved")
        {
            risk_keys.insert(identity(finding)?);
            if let Some(id) = finding["control_gap_id"]
                .as_str()
                .filter(|id| !id.is_empty())
            {
                promoted.insert(id);
            }
        }
    }
    let mut ordered = gaps
        .iter()
        .map(|gap| {
            Ok((
                gap_weight(gap, &rules)?,
                text(gap, "rule_id")?,
                text(gap, "id")?,
                gap,
            ))
        })
        .collect::<Result<Vec<_>, Error>>()?;
    ordered.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)).then(a.2.cmp(b.2)));
    let mut seen = HashSet::new();
    let mut seen_ids = HashSet::new();
    let mut categories: BTreeMap<String, Decimal> = BTreeMap::new();
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut contributions = Vec::new();
    let mut total = Decimal::ZERO;
    for (weight, rule, id, gap) in ordered {
        let key = identity(gap)?;
        let category = text(&rules[rule], "category")?;
        let status = text(gap, "status")?;
        let mut amount = Decimal::ZERO;
        let reason = if promoted.contains(id) || risk_keys.contains(&key) {
            "included in risk root"
        } else if seen_ids.contains(id) || seen.contains(&key) {
            "duplicate control gap"
        } else if weight.is_zero() {
            status
        } else {
            seen.insert(key);
            seen_ids.insert(id);
            let count = counts.entry(category.into()).or_default();
            *count += 1;
            let subtotal = categories.entry(category.into()).or_default();
            amount = (weight / Decimal::from(*count))
                .min(Decimal::from(5) - *subtotal)
                .min(Decimal::from(25) - total);
            *subtotal += amount;
            total += amount;
            "scored"
        };
        contributions.push(json!({
            "id": id, "rule_id": rule, "category": category, "status": status,
            "deduction": number(amount)?, "reason": reason
        }));
    }
    let categories = categories
        .into_iter()
        .map(|(key, value)| Ok((key, number(value)?)))
        .collect::<Result<BTreeMap<_, _>, Error>>()?;
    Ok((
        json!({
            "penalty": number(total)?, "contributions": contributions, "categories": categories,
            "category_cap": 5, "total_cap": 25, "decay": "harmonic", "uncertain_multiplier": 0.5
        }),
        total,
    ))
}

pub(crate) fn score_findings(
    findings: &[Value],
    failures: &[String],
    gaps: &[Value],
) -> Result<Value, Error> {
    let mut ordered = findings
        .iter()
        .map(|finding| Ok((deduction(finding)?, finding)))
        .collect::<Result<Vec<_>, Error>>()?;
    // Stable ordering preserves the original finding when duplicate roots have equal weights.
    ordered.sort_by(|a, b| b.0.cmp(&a.0));
    let mut contributions = Vec::new();
    let mut seen = HashSet::new();
    let mut total = Decimal::ZERO;
    for (amount, finding) in ordered {
        if text(finding, "scope")? == "environment" || !seen.insert(identity(finding)?) {
            continue;
        }
        total += amount;
        contributions.push(json!({
            "id": finding["id"], "title": finding["title"], "deduction": number(amount)?,
            "reason": finding["reason"], "refs": finding["refs"]
        }));
    }
    let risk_score = rounded((Decimal::from(100) - total).max(Decimal::ZERO))?;
    let (hygiene, penalty) = score_gaps(gaps, findings)?;
    let ordinary = rounded((Decimal::from(100) - total - penalty).max(Decimal::ZERO))?;
    let critical = findings
        .iter()
        .filter(|f| {
            f["severity"] == "Critical"
                && f["scope"] == "skill"
                && f["status"] != "accepted"
                && f["status"] != "refuted"
        })
        .collect::<Vec<_>>();
    let confirmed = critical
        .iter()
        .filter(|f| f["critical_confirmed"] == true)
        .collect::<Vec<_>>();
    let (decision, score, reasons) = if !confirmed.is_empty() {
        (
            "blocked",
            json!(0.0),
            confirmed
                .iter()
                .map(|f| Ok(format!("confirmed Critical: {}", text(f, "id")?)))
                .collect::<Result<Vec<_>, Error>>()?,
        )
    } else if !critical.is_empty() {
        (
            "needs_review",
            Value::Null,
            critical
                .iter()
                .map(|f| Ok(format!("unresolved Critical: {}", text(f, "id")?)))
                .collect::<Result<Vec<_>, Error>>()?,
        )
    } else if !failures.is_empty() {
        ("inconclusive", Value::Null, failures.to_vec())
    } else {
        ("scored", json!(ordinary), Vec::new())
    };
    Ok(json!({
        "decision": decision, "score": score, "rankable": decision == "scored",
        "noncritical_score": ordinary, "risk_score": risk_score, "hygiene": hygiene,
        "reasons": reasons, "contributions": contributions,
        "parameters": {
            "severity": {"High": 15, "Medium": 10, "Low": 5},
            "existence": {"material_risk": "1", "unresolved": "0.5", "control_gap": "0", "accepted": "0", "refuted": "0"},
            "dynamic": {"confirmed_impact": "1", "attempted_violation": "0.8", "contained": "0.3", "no_proof": "0.6", "not_activated": "0.6", "unsupported": "0.6"}
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_frozen_python_oracle() -> Result<(), Error> {
        fn compare(actual: &Value, expected: &Value) {
            match (actual, expected) {
                (Value::Number(a), Value::Number(b)) => {
                    let difference = (a.as_f64().unwrap_or(f64::INFINITY)
                        - b.as_f64().unwrap_or(f64::NEG_INFINITY))
                    .abs();
                    assert!(difference < 1e-12, "number mismatch: {a} vs {b}");
                }
                (Value::Array(a), Value::Array(b)) => {
                    assert_eq!(a.len(), b.len());
                    for (a, b) in a.iter().zip(b) {
                        compare(a, b);
                    }
                }
                (Value::Object(a), Value::Object(b)) => {
                    assert_eq!(a.len(), b.len());
                    for (key, value) in b {
                        compare(&a[key], value);
                    }
                }
                _ => assert_eq!(actual, expected),
            }
        }
        let cases: Value = serde_json::from_str(include_str!("../assets/score-oracle.json"))
            .map_err(|error| Error::Contract(error.to_string()))?;
        for case in cases.as_array().into_iter().flatten() {
            let failures = case["failures"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let actual = score_findings(
                case["findings"].as_array().map_or(&[], Vec::as_slice),
                &failures,
                case["gaps"].as_array().map_or(&[], Vec::as_slice),
            )?;
            // Internal harmonic contributions allow only conversion-level float error;
            // the public decimal-rounded score must match exactly.
            assert_eq!(actual["score"], case["expected"]["score"]);
            compare(&actual, &case["expected"]);
        }
        Ok(())
    }

    fn finding() -> Value {
        json!({"id":"r1", "title":"boundary", "severity":"High", "status":"material_risk",
            "entry":"run.py", "operation":"delete", "boundary":"work -> guard", "reason":"unchecked input",
            "refs":["source:run.py:1"], "scope":"skill", "dynamic":"no_proof", "critical_confirmed":false})
    }

    #[test]
    fn frozen_dynamic_weights_and_decimal_rounding() -> Result<(), Error> {
        for (state, expected) in [
            ("confirmed_impact", 85.0),
            ("attempted_violation", 88.0),
            ("no_proof", 91.0),
            ("contained", 95.5),
            ("not_activated", 91.0),
            ("unsupported", 91.0),
        ] {
            let mut f = finding();
            f["dynamic"] = json!(state);
            assert_eq!(score_findings(&[f], &[], &[])?["score"], expected);
        }
        let mut f = finding();
        f["status"] = json!("unresolved");
        f["dynamic"] = json!("contained");
        assert_eq!(score_findings(&[f], &[], &[])?["score"], 97.8);
        Ok(())
    }

    #[test]
    fn critical_gates_precede_execution_failure() -> Result<(), Error> {
        let mut f = finding();
        f["severity"] = json!("Critical");
        assert_eq!(
            score_findings(&[f.clone()], &["timeout".into()], &[])?["decision"],
            "needs_review"
        );
        f["critical_confirmed"] = json!(true);
        assert_eq!(
            score_findings(&[f.clone()], &["timeout".into()], &[])?["score"],
            0.0
        );
        f["scope"] = json!("environment");
        assert_eq!(score_findings(&[f], &[], &[])?["score"], 100.0);
        assert_eq!(
            score_findings(&[], &["timeout".into()], &[])?["decision"],
            "inconclusive"
        );
        Ok(())
    }

    #[test]
    fn deduplicates_unicode_root_and_keeps_greatest_weight() -> Result<(), Error> {
        let mut first = finding();
        first["entry"] = json!(" Straße  ");
        let mut duplicate = finding();
        duplicate["entry"] = json!("STRASSE");
        duplicate["dynamic"] = json!("confirmed_impact");
        duplicate["id"] = json!("higher");
        let result = score_findings(&[first, duplicate], &[], &[])?;
        assert_eq!(result["score"], 85.0);
        assert_eq!(result["contributions"].as_array().map(Vec::len), Some(1));
        assert_eq!(result["contributions"][0]["id"], "higher");
        Ok(())
    }

    #[test]
    fn hygiene_promotions_decay_duplicates_and_category_cap() -> Result<(), Error> {
        let gap = |id: &str, entry: &str| {
            json!({"id":id,"rule_id":"LP001","status":"confirmed",
            "entry":entry,"operation":"delete","boundary":"work -> guard"})
        };
        let gaps = vec![
            gap("promoted", "run.py"),
            gap("a", "a"),
            gap("b", "b"),
            gap("duplicate", "b"),
            gap("c", "c"),
        ];
        let result = score_findings(&[finding()], &[], &gaps)?;
        assert_eq!(result["risk_score"], 91.0);
        assert_eq!(result["score"], 86.0);
        assert_eq!(result["hygiene"]["penalty"], 5.0);
        let reasons = result["hygiene"]["contributions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| entry["reason"].as_str())
            .collect::<Vec<_>>();
        assert!(reasons.contains(&"included in risk root"));
        assert!(reasons.contains(&"duplicate control gap"));
        Ok(())
    }
}
