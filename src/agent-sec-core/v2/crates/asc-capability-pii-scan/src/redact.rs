//! Redact merged character spans without exposing overlapping sensitive tails.

use crate::models::{Candidate, Severity};
use crate::report::{OMITTED_TEXT, REPORT_BYTES};
use crate::validators::decimal;
use std::ops::Range;

pub(crate) fn value(value: &str, kind: &str, category: &str) -> String {
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
            let digits: String = value.chars().filter(|c| decimal(*c).is_some()).collect();
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
            let digits: String = value.chars().filter(|c| decimal(*c).is_some()).collect();
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
        "cn_id" if value.chars().take(7).count() == 7 => format!(
            "{}***********{}",
            value.chars().take(3).collect::<String>(),
            tail(value)
        ),
        "cn_id" => "[REDACTED_CN_ID]".into(),
        "private_key" => "[REDACTED_PRIVATE_KEY]".into(),
        "api_key"
        | "bearer_token"
        | "jwt"
        | "aliyun_access_key_id"
        | "aliyun_access_key_secret"
        | "generic_secret_field" => {
            if value.chars().take(9).count() <= 8 {
                "[REDACTED]".into()
            } else {
                format!(
                    "{}...[REDACTED]...{}",
                    value.chars().take(4).collect::<String>(),
                    tail(value)
                )
            }
        }
        _ => "[REDACTED]".into(),
    }
}

fn tail(value: &str) -> String {
    value
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

// Ordered matches need only the current transitive overlap group. Once the
// output is too large, discard the entire buffer, never return an untouched tail.
pub(crate) struct Text<'a> {
    input: &'a str,
    output: Option<String>,
    cursor: usize,
    group: Option<Group<'a>>,
}

struct Group<'a> {
    first: Range<usize>,
    end: usize,
    mixed: bool,
    priority: (bool, bool, &'a str),
    replacement: String,
}

impl<'a> Text<'a> {
    pub(crate) fn new(input: &'a str) -> Self {
        Self {
            input,
            output: Some(String::new()),
            cursor: 0,
            group: None,
        }
    }

    pub(crate) fn push(&mut self, candidate: &Candidate<'a>) {
        if self.output.is_none() {
            return;
        }
        let priority = (
            candidate.category != "custom",
            candidate.severity != Severity::Deny,
            candidate.kind,
        );
        if let Some(group) = self
            .group
            .as_mut()
            .filter(|g| candidate.bytes.start < g.end)
        {
            group.end = group.end.max(candidate.bytes.end);
            group.mixed |= group.first != candidate.bytes;
            if priority < group.priority {
                group.priority = priority;
                group.replacement = value(candidate.value, candidate.kind, candidate.category);
            }
            return;
        }
        self.flush();
        if self.output.is_some() {
            self.group = Some(Group {
                first: candidate.bytes.clone(),
                end: candidate.bytes.end,
                mixed: false,
                priority,
                replacement: value(candidate.value, candidate.kind, candidate.category),
            });
        }
    }

    fn append(&mut self, value: &str) {
        if let Some(output) = &mut self.output {
            if value.len() <= REPORT_BYTES.saturating_sub(output.len()) {
                output.push_str(value);
            } else {
                self.output = None;
            }
        }
    }

    fn flush(&mut self) {
        if let Some(group) = self.group.take() {
            self.append(&self.input[self.cursor..group.first.start]);
            if group.mixed {
                self.append(&format!(
                    "[{}_REDACTED]",
                    group.priority.2.to_ascii_uppercase()
                ));
            } else {
                self.append(&group.replacement);
            }
            self.cursor = group.end;
        }
    }

    pub(crate) fn finish(mut self) -> (String, bool) {
        self.flush();
        self.append(&self.input[self.cursor..]);
        match self.output {
            Some(text) => (text, false),
            None => (OMITTED_TEXT.into(), true),
        }
    }
}
