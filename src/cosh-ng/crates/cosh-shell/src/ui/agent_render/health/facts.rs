use super::*;

pub(super) fn format_health_message(
    i18n: crate::I18n,
    id: HealthMessageId,
    args: &BTreeMap<String, String>,
) -> String {
    crate::diagnostics::health::prompt_selection::format_health_message(i18n, id, args)
}

pub(super) fn number_fact(facts: &[HealthFact], key: &str) -> Option<f64> {
    facts
        .iter()
        .find(|fact| fact.key == key)
        .and_then(|fact| match &fact.value {
            HealthFactValue::Integer(value) => Some(*value as f64),
            HealthFactValue::Unsigned(value) => Some(*value as f64),
            HealthFactValue::Float(value) => Some(*value),
            HealthFactValue::String(_) | HealthFactValue::Bool(_) => None,
        })
}

pub(super) fn string_fact<'a>(facts: &'a [HealthFact], key: &str) -> Option<&'a str> {
    facts
        .iter()
        .find(|fact| fact.key == key)
        .and_then(|fact| match &fact.value {
            HealthFactValue::String(value) => Some(value.as_str()),
            HealthFactValue::Integer(_)
            | HealthFactValue::Unsigned(_)
            | HealthFactValue::Float(_)
            | HealthFactValue::Bool(_) => None,
        })
}

pub(super) fn number_from_fact(fact: &HealthFact) -> Option<f64> {
    match &fact.value {
        HealthFactValue::Integer(value) => Some(*value as f64),
        HealthFactValue::Unsigned(value) => Some(*value as f64),
        HealthFactValue::Float(value) => Some(*value),
        HealthFactValue::String(_) | HealthFactValue::Bool(_) => None,
    }
}

pub(super) fn string_from_fact(fact: &HealthFact) -> Option<&str> {
    match &fact.value {
        HealthFactValue::String(value) => Some(value.as_str()),
        HealthFactValue::Integer(_)
        | HealthFactValue::Unsigned(_)
        | HealthFactValue::Float(_)
        | HealthFactValue::Bool(_) => None,
    }
}

pub(super) fn format_age_seconds(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h", seconds / 3600)
    }
}

pub(super) fn format_oom_age(seconds: u64, i18n: crate::I18n) -> String {
    let age = format_age_seconds(seconds);
    i18n.format(crate::MessageId::HealthEvidenceOomAge, &[("age", &age)])
}

pub(super) fn load_evidence_label(key: &str, i18n: crate::I18n) -> String {
    let label_id = match key {
        "cpu.load_per_core_5m" => crate::MessageId::HealthMetricLoad5mShort,
        _ => crate::MessageId::HealthMetricLoad1mShort,
    };
    let compact = i18n.t(label_id);
    let load = i18n.t(crate::MessageId::HealthMetricPressure);
    if is_zh(i18n) {
        format!("{compact}{load}")
    } else {
        format!("{load} {compact}")
    }
}

pub(super) fn oom_scope_label_from_constraint(raw: &str, i18n: crate::I18n) -> String {
    oom_scope_label_from_id(oom_scope_label_id_from_constraint(raw), i18n)
}

pub(super) fn oom_scope_label_id_from_constraint(raw: &str) -> &'static str {
    match raw {
        "CONSTRAINT_MEMCG" => "memcg",
        "CONSTRAINT_NONE" => "host",
        "CONSTRAINT_CPUSET" => "cpuset",
        "CONSTRAINT_MEMORY_POLICY" => "memory_policy",
        _ => "unknown",
    }
}

pub(super) fn oom_scope_label_from_id(id: &str, i18n: crate::I18n) -> String {
    let message_id = match id {
        "memcg" => crate::MessageId::HealthOomScopeMemcg,
        "host" => crate::MessageId::HealthOomScopeHost,
        "cpuset" => crate::MessageId::HealthOomScopeCpuset,
        "memory_policy" => crate::MessageId::HealthOomScopeMemoryPolicy,
        _ => crate::MessageId::HealthOomScopeUnknown,
    };
    i18n.t(message_id).to_string()
}
