//! Compare policy edits without admitting native effects or changing a running instance.

use crate::{Configuration, Error};
use serde_json::Value;

impl Configuration {
    /// Check that a candidate changes only Providers, events or the default event budget.
    ///
    /// Both documents have already passed syntax, schema and static reference
    /// validation. Agent launch, service, audit and observation settings must
    /// remain identical, including the presence of optional fields. Comparison
    /// uses parsed values, so formatting and mapping order do not matter.
    /// Schema integers compare by value across decimal and exponent spellings.
    ///
    /// This is an offline scope check. It does not prepare Providers, verify a
    /// native bridge or idle boundary, or activate a policy on any instance.
    ///
    /// # Errors
    /// Returns [`Error::Invariant`] at the changed section if an edit requires
    /// another launch. Diagnostics never include configuration values.
    pub fn validate_policy_update(&self, candidate: &Configuration) -> Result<(), Error> {
        unchanged_except(self.as_value(), candidate.as_value(), "", &["spec"])?;
        unchanged_except(
            &self.as_value()["spec"],
            &candidate.as_value()["spec"],
            "/spec",
            &["providers", "events", "execution"],
        )?;
        unchanged_except(
            &self.as_value()["spec"]["execution"],
            &candidate.as_value()["spec"]["execution"],
            "/spec/execution",
            &["default_event_budget_ms"],
        )
    }
}

fn unchanged_except(
    baseline: &Value,
    candidate: &Value,
    path: &str,
    allowed: &[&str],
) -> Result<(), Error> {
    let invalid = || Error::Invariant {
        path: path.into(),
        reason: "policy update comparison requires a configuration object",
    };
    let baseline = baseline.as_object().ok_or_else(invalid)?;
    let candidate = candidate.as_object().ok_or_else(invalid)?;
    // Compare both key sets so adding or removing a frozen section also fails.
    for key in baseline.keys().chain(candidate.keys()) {
        if !allowed.contains(&key.as_str())
            && !baseline
                .get(key)
                .zip(candidate.get(key))
                .is_some_and(|(baseline, candidate)| same_value(baseline, candidate))
        {
            return Err(Error::Invariant {
                path: format!("{path}/{key}"),
                reason: "policy updates cannot change this section; use another launch",
            });
        }
    }
    Ok(())
}

fn same_value(baseline: &Value, candidate: &Value) -> bool {
    match (baseline, candidate) {
        (Value::Object(baseline), Value::Object(candidate)) => {
            baseline.len() == candidate.len()
                && baseline.iter().all(|(key, value)| {
                    candidate
                        .get(key)
                        .is_some_and(|next| same_value(value, next))
                })
        }
        (Value::Array(baseline), Value::Array(candidate)) => {
            baseline.len() == candidate.len()
                && baseline
                    .iter()
                    .zip(candidate)
                    .all(|(value, next)| same_value(value, next))
        }
        (Value::Number(baseline), Value::Number(candidate)) => {
            baseline == candidate
                || schema_integer(baseline)
                    .zip(schema_integer(candidate))
                    .is_some_and(|(value, next)| value == next)
        }
        _ => baseline == candidate,
    }
}

fn schema_integer(value: &serde_json::Number) -> Option<u64> {
    // Frozen schema integers fit u32. Bound conversion so large adjacent integers
    // cannot round to the same f64 and accidentally compare equal.
    value
        .as_f64()
        .filter(|value| *value >= 1.0 && *value <= f64::from(u32::MAX) && value.fract() == 0.0)
        .map(|value| value as u64)
}
