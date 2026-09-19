//! Redact merged character spans without exposing overlapping sensitive tails.

use crate::models::{PiiFinding, Severity};
use crate::validators::decimal;

pub(crate) fn value(value: &str, kind: &str, category: &str) -> String {
    let chars: Vec<_> = value.chars().collect();
    let digits: String = value.chars().filter(|c| decimal(*c).is_some()).collect();
    if category == "custom" {
        return format!("[{}_REDACTED]", kind.to_ascii_uppercase());
    }
    match kind {
        "email" => value.split_once('@').map_or_else(
            || "[REDACTED_EMAIL]".into(),
            |(local, domain)| {
                format!(
                    "{}***@{domain}",
                    local
                        .chars()
                        .next()
                        .map_or(String::new(), |c| c.to_string())
                )
            },
        ),
        "phone_cn" => {
            let digits: Vec<_> = digits.chars().collect();
            if digits.len() < 11 {
                return "[REDACTED_PHONE]".into();
            }
            let core = &digits[digits.len() - 11..];
            format!(
                "{}****{}",
                core[..3].iter().collect::<String>(),
                core[7..].iter().collect::<String>()
            )
        }
        "credit_card" => {
            let last: String = digits
                .chars()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            if last.chars().count() < 4 {
                "[REDACTED_CARD]".into()
            } else {
                format!("[REDACTED_CARD:{last}]")
            }
        }
        "cn_id" if chars.len() >= 7 => format!(
            "{}***********{}",
            chars[..3].iter().collect::<String>(),
            chars[chars.len() - 4..].iter().collect::<String>()
        ),
        "cn_id" => "[REDACTED_CN_ID]".into(),
        "private_key" => "[REDACTED_PRIVATE_KEY]".into(),
        "api_key"
        | "bearer_token"
        | "jwt"
        | "aliyun_access_key_id"
        | "aliyun_access_key_secret"
        | "generic_secret_field" => {
            if chars.len() <= 8 {
                "[REDACTED]".into()
            } else {
                format!(
                    "{}...[REDACTED]...{}",
                    chars[..4].iter().collect::<String>(),
                    chars[chars.len() - 4..].iter().collect::<String>()
                )
            }
        }
        _ => "[REDACTED]".into(),
    }
}

pub(crate) fn text(input: &str, findings: &[PiiFinding]) -> String {
    let chars: Vec<_> = input.chars().collect();
    let mut ordered: Vec<_> = findings.iter().collect();
    ordered.sort_by_key(|f| (f.span, &f.pii_type));
    let mut result = String::new();
    let mut cursor = 0;
    let mut index = 0;
    while index < ordered.len() {
        let first = ordered[index];
        let mut end = first.span.end;
        let mut group_end = index + 1;
        while group_end < ordered.len() && ordered[group_end].span.start < end {
            end = end.max(ordered[group_end].span.end);
            group_end += 1;
        }
        // Findings originate in the matcher and use character offsets into input.
        result.extend(chars[cursor..first.span.start].iter());
        let selected = ordered[index..group_end]
            .iter()
            .copied()
            .min_by_key(|f| {
                (
                    f.category != "custom",
                    f.severity != Severity::Deny,
                    &f.pii_type,
                )
            })
            .unwrap_or(first);
        if ordered[index..group_end]
            .iter()
            .any(|f| f.span != first.span)
        {
            result.push('[');
            result.push_str(&selected.pii_type.to_ascii_uppercase());
            result.push_str("_REDACTED]");
        } else {
            result.push_str(&selected.evidence_redacted);
        }
        cursor = end;
        index = group_end;
    }
    result.extend(chars[cursor..].iter());
    result
}
