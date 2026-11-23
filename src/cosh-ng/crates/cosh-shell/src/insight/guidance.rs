use serde::{Deserialize, Serialize};

use crate::config::guide::GuideSnapshot;

pub(crate) mod quote;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BashEvent {
    pub(crate) expression: String,
    pub(crate) status: u16,
    pub(crate) cwd: String,
    pub(crate) observed_at_ms: u64,
    pub(crate) scope: ExecutionScope,
    pub(crate) identity: CommandIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecutionScope {
    ObservedCommandFragment,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommandIdentity {
    ExternalUnshadowed,
    Alias,
    Function,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureKind {
    CargoBuild,
    CargoTest,
    Pytest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SkipReason {
    Disabled,
    UnsupportedCommand,
    UnprovenScope,
    UnprovenIdentity,
    InvalidEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    Skip(SkipReason),
    SuggestOnly(FailureKind),
}

pub(crate) fn decide(event: &BashEvent, state: &GuideSnapshot) -> Decision {
    if !state.preferences.guide_enabled {
        return Decision::Skip(SkipReason::Disabled);
    }
    if event.cwd.len() > 4096 || !event.cwd.starts_with('/') || event.cwd.contains('\0') {
        return Decision::Skip(SkipReason::InvalidEvidence);
    }
    if event.scope != ExecutionScope::ObservedCommandFragment {
        return Decision::Skip(SkipReason::UnprovenScope);
    }
    if event.identity != CommandIdentity::ExternalUnshadowed {
        return Decision::Skip(SkipReason::UnprovenIdentity);
    }
    match failure_kind(event) {
        Some(kind) => Decision::SuggestOnly(kind),
        None => Decision::Skip(SkipReason::UnsupportedCommand),
    }
}

pub(crate) fn failure_kind(event: &BashEvent) -> Option<FailureKind> {
    if event.scope != ExecutionScope::ObservedCommandFragment
        || event.identity != CommandIdentity::ExternalUnshadowed
        || event.expression.len() > 1024
        || event.cwd.len() > 4096
        || !event.cwd.starts_with('/')
        || event.cwd.chars().any(char::is_control)
        || crate::evidence::redact_sensitive_text(&event.expression).1
        || crate::evidence::redact_sensitive_text(&event.cwd).1
    {
        return None;
    }
    let words = literal_words(&event.expression)?;
    if crate::evidence::redact_sensitive_text(&words.join(" ")).1 {
        return None;
    }
    if words.iter().any(|word| {
        matches!(
            word.as_str(),
            "--help" | "-h" | "--version" | "-V" | "--no-run" | "--collect-only" | "--co"
        )
    }) {
        return None;
    }
    match (
        words.first()?.as_str(),
        words.get(1).map(String::as_str),
        event.status,
    ) {
        ("cargo", Some("build"), 101) => Some(FailureKind::CargoBuild),
        ("cargo", Some("test"), 101) => Some(FailureKind::CargoTest),
        ("pytest", _, 1) => Some(FailureKind::Pytest),
        _ => None,
    }
}

fn literal_words(expression: &str) -> Option<Vec<String>> {
    if expression.is_empty() || expression.starts_with(' ') || expression.ends_with(' ') {
        return None;
    }
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut started = false;
    for ch in expression.chars() {
        if ch.is_control() || matches!(ch, '$' | '`' | '\\') {
            return None;
        }
        if let Some(delimiter) = quote {
            if ch == delimiter {
                quote = None;
            } else {
                word.push(ch);
            }
        } else if matches!(ch, '\'' | '"') {
            quote = Some(ch);
            started = true;
        } else if ch == ' ' {
            if started {
                words.push(std::mem::take(&mut word));
                started = false;
            }
        } else if ch.is_alphanumeric() || "_-./:=,@+%".contains(ch) {
            word.push(ch);
            started = true;
        } else {
            return None;
        }
    }
    if quote.is_some() {
        return None;
    }
    if started {
        words.push(word);
    }
    (words.len() <= 64).then_some(words)
}

pub(crate) fn external_failure_hint(event: &BashEvent) -> String {
    let facts = serde_json::json!({
        "source": "external_bash", "command_fragment": event.expression,
        "original_cwd": event.cwd, "exit_status": event.status,
        "observed_at_ms": event.observed_at_ms, "scope": event.scope,
        "output_captured": false, "expanded_argv_captured": false,
    });
    format!("external_bash_failure\n{facts}")
}

#[cfg(test)]
#[path = "guidance/tests.rs"]
mod tests;
