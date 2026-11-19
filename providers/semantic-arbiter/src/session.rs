//! Native user prompts captured per session become the authoritative task text.

use semantic_arbiter::deadline::Ingress;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::mapping;
use crate::storage;

/// Core contract limit on authoritative task text.
const MAX_TEXT: usize = 16_384;
/// Bounds the stored turn list so `task.json` stays within its size limit.
const MAX_TURNS: usize = 64;
const SEPARATOR: &str = "\n\n";

/// Ordered prompts of one native session, keyed by the opaque session identity.
#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionTask {
    turns: Vec<Turn>,
    /// Set once the joined text would exceed the contract limit; never truncated.
    overflowed: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Turn {
    /// Domain-separated digest of the native turn identity, used to drop redelivery.
    turn: String,
    text: String,
}

/// Joined task text of a session together with the number of turns it spans.
pub(crate) struct CapturedTask {
    pub text: String,
    pub turns: usize,
}

impl SessionTask {
    fn joined_len(&self) -> usize {
        self.turns.iter().map(|turn| turn.text.len()).sum::<usize>()
            + SEPARATOR.len() * self.turns.len().saturating_sub(1)
    }

    /// Appends one turn; returns `None` for a redelivered turn.
    fn append(&mut self, turn: String, text: &str) -> Option<&'static str> {
        if self.turns.iter().any(|existing| existing.turn == turn) {
            return None;
        }
        if self.overflowed {
            return Some("task_too_large");
        }
        let separator = if self.turns.is_empty() {
            0
        } else {
            SEPARATOR.len()
        };
        if self.turns.len() >= MAX_TURNS || self.joined_len() + separator + text.len() > MAX_TEXT {
            self.overflowed = true;
            return Some("task_too_large");
        }
        self.turns.push(Turn {
            turn,
            text: text.to_owned(),
        });
        Some("task_captured")
    }

    pub(crate) fn task(&self) -> Result<CapturedTask, &'static str> {
        if self.overflowed {
            return Err("task_too_large");
        }
        if self.turns.is_empty() {
            return Err("task_unavailable");
        }
        let texts: Vec<_> = self.turns.iter().map(|turn| turn.text.as_str()).collect();
        Ok(CapturedTask {
            text: texts.join(SEPARATOR),
            turns: texts.len(),
        })
    }
}

/// Plain digest of the task bytes, reproducible with `sha256sum`.
pub(crate) fn digest(text: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(text.as_bytes()))
}

/// Records one `input.submit` prompt in its session; never contacts the model.
pub(crate) fn capture(request: &Value, config: &Config, ingress: Ingress) -> &'static str {
    let budget = match mapping::budget(request, config, ingress) {
        Ok(budget) => budget,
        Err(code) => return code,
    };
    let event = &request["event"];
    let session_id = match mapping::context(request, config, "input.submit") {
        Ok(session_id) => session_id,
        Err(code) => return code,
    };
    let turn = match event["turn_id"]
        .as_str()
        .filter(|turn| !turn.trim().is_empty() && turn.len() <= 512)
    {
        Some(turn) => turn,
        None => return "missing_identity",
    };
    let Some(text) = event["input"]["text"].as_str() else {
        return "unsupported_input";
    };
    if text.trim().is_empty() {
        return "empty_input";
    }
    let turn = match mapping::identity("turn", &Value::from(turn)) {
        Ok(turn) => turn,
        Err(code) => return code,
    };
    let deadline = ingress.deadline_at(budget.deadline_ms, config.model.timeout_ms);
    let mut code = "turn_duplicate";
    let updated = storage::update_session(config, &session_id, deadline, |task| {
        task.append(turn, text)
            .inspect(|outcome| code = *outcome)
            .is_some()
    });
    match updated {
        Ok(()) => code,
        Err(error) => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turns_join_in_order_and_overflow_is_sticky() {
        let mut task = SessionTask::default();
        assert_eq!(task.task().err(), Some("task_unavailable"));
        assert_eq!(task.append("a".into(), "first"), Some("task_captured"));
        assert_eq!(task.append("a".into(), "again"), None);
        assert_eq!(task.append("b".into(), "second"), Some("task_captured"));
        assert_eq!(task.task().unwrap().text, "first\n\nsecond");
        assert_eq!(task.task().unwrap().turns, 2);
        let rest = MAX_TEXT - task.joined_len() - SEPARATOR.len();
        assert_eq!(
            task.append("c".into(), &"x".repeat(rest)),
            Some("task_captured")
        );
        assert_eq!(task.task().unwrap().text.len(), MAX_TEXT);
        assert_eq!(task.append("d".into(), "y"), Some("task_too_large"));
        assert_eq!(task.task().err(), Some("task_too_large"));
        assert_eq!(task.append("e".into(), "z"), Some("task_too_large"));
        assert_eq!(task.turns.len(), 3);
    }

    #[test]
    fn turn_count_is_bounded() {
        let mut task = SessionTask::default();
        for index in 0..MAX_TURNS {
            assert_eq!(task.append(index.to_string(), "t"), Some("task_captured"));
        }
        assert_eq!(task.append("last".into(), "t"), Some("task_too_large"));
    }
}
