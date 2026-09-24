//! Skill business contracts shared by clients, daemon applications and capabilities.

mod command;
mod identity;

pub use command::SkillSecCommand;
pub use identity::SkillIdentity;
use serde::{Deserialize, Serialize};

/// Invalid transport-independent Skill value or command selection.
#[derive(Debug, thiserror::Error)]
pub enum SkillSecInputError {
    /// A value violates the supported command or lexical identity contract.
    #[error("invalid SkillSec input: {0}")]
    Invalid(String),
}

/// Invocation input constructed by the trusted application boundary, never decoded as a whole.
pub struct SkillSecRequest {
    /// Closed business operation; contains no physical directory mapping.
    pub command: SkillSecCommand,
    /// Authenticated caller UID used for privileged key and export operations.
    pub caller_uid: u32,
}

/// Persisted user decision, distinct from a Hook's one-operation confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionAction {
    /// Approve this version.
    Allow,
    /// Persist the existing always-allow behavior.
    AlwaysAllow,
    /// Hide this Skill.
    Block,
    /// Restore a selected trusted version.
    Rollback,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shared_contract_preserves_wire_names_and_rejects_caller_authority() {
        let command: SkillSecCommand = serde_json::from_value(json!({
            "command": "decide", "skillDir": "/srv/skills/example",
            "action": "always_allow", "reason": "reviewed"
        }))
        .unwrap();
        assert_eq!(command.name(), "decide");
        assert_eq!(
            command.identities(&[]).unwrap()[0].path().to_str(),
            Some("/srv/skills/example")
        );
        let encoded = serde_json::to_value(command).unwrap();
        assert_eq!(encoded["action"], "always_allow");
        assert_eq!(encoded["skillDir"], "/srv/skills/example");
        for value in [
            json!({"command": "rotate-keys", "callerUid": 0}),
            json!({"command": "status", "ioDir": "/private"}),
            json!({"command": "reconcile", "skillDir": "/srv/skills/example"}),
        ] {
            assert!(serde_json::from_value::<SkillSecCommand>(value).is_err());
        }
    }

    #[test]
    fn shared_identity_validation_remains_lexical_and_strict() {
        for invalid in [
            "/",
            "relative",
            "/srv//skill",
            "/srv/../skill",
            "/srv/./skill",
            "/srv/skill/",
        ] {
            assert!(SkillIdentity::new(invalid).is_err(), "{invalid}");
            assert!(
                serde_json::from_value::<SkillIdentity>(json!(invalid)).is_err(),
                "{invalid}"
            );
        }
        let identity = SkillIdentity::new("/srv/nonexistent/skill").unwrap();
        assert_eq!(identity.name(), "skill");
        assert_eq!(
            serde_json::to_value(identity).unwrap(),
            "/srv/nonexistent/skill"
        );
    }
}
