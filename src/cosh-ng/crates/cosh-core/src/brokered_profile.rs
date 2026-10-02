//! Private brokered-profile handshake values owned by the Core binary.
//!
//! Gateway remains authoritative for admission. Core mirrors only the closed
//! private wire values it must reject before accepting a brokered user turn.

use serde::{Deserialize, Serialize};

const TASK_ONLY_V1_PROFILE: &str = "task-only-v1";
const TASK_ONLY_V1_MANIFEST_DIGEST: &str =
    "2b95e0f3e28df8eb2b7930f2dec3650ffe399f971671c971865e4663c382c94a";
const TASK_ONLY_V1_RUNTIME_TOOLS: &[&str] = &["ask_user_question"];
const WORKSPACE_CHECKPOINT_V1_PROFILE: &str = "workspace-checkpoint-v1";
const WORKSPACE_CHECKPOINT_V1_MANIFEST_DIGEST: &str =
    "6b3e7093e7b8656d4a7cf21faa85b9eed761ef415d002623cfc442f3ef3c8ae1";
const WORKSPACE_CHECKPOINT_V1_RUNTIME_TOOLS: &[&str] =
    &["ask_user_question", "workspace_checkpoint_create"];
const WORKSPACE_WRITE_V1_PROFILE: &str = "workspace-write-v1";
const WORKSPACE_WRITE_V1_MANIFEST_DIGEST: &str =
    "30574302eeba3adbb5ea143a8a869331d58a15bd24b9532d0f52613136bb2b2a";
const WORKSPACE_WRITE_V1_RUNTIME_TOOLS: &[&str] = &["ask_user_question", "write_file"];

/// Profile identity carried by the private brokered Core wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokeredCapabilityProfileIdentity {
    /// Exact versioned profile name selected by Gateway.
    pub profile_id: String,
    /// Exact canonical manifest digest selected by Gateway.
    pub manifest_digest: String,
}

impl BrokeredCapabilityProfileIdentity {
    /// Returns the only capability profile understood by private Core v3.
    pub fn task_only_v1() -> Self {
        Self {
            profile_id: TASK_ONLY_V1_PROFILE.to_owned(),
            manifest_digest: TASK_ONLY_V1_MANIFEST_DIGEST.to_owned(),
        }
    }

    /// Returns the closed checkpoint profile understood by private Core v4.
    pub fn workspace_checkpoint_v1() -> Self {
        Self {
            profile_id: WORKSPACE_CHECKPOINT_V1_PROFILE.to_owned(),
            manifest_digest: WORKSPACE_CHECKPOINT_V1_MANIFEST_DIGEST.to_owned(),
        }
    }

    /// Returns the closed workspace-write profile understood by private Core v5.
    pub fn workspace_write_v1() -> Self {
        Self {
            profile_id: WORKSPACE_WRITE_V1_PROFILE.to_owned(),
            manifest_digest: WORKSPACE_WRITE_V1_MANIFEST_DIGEST.to_owned(),
        }
    }

    /// Verifies that the requested identity is the closed private v3 profile.
    pub fn verify_task_only_v1(&self) -> Result<(), &'static str> {
        if self.profile_id != TASK_ONLY_V1_PROFILE {
            return Err("capability profile identity does not match the brokered profile");
        }
        if self.manifest_digest != TASK_ONLY_V1_MANIFEST_DIGEST {
            return Err("capability profile manifest digest does not match the brokered profile");
        }
        Ok(())
    }

    /// Verifies that the requested identity is the closed private v4 profile.
    pub fn verify_workspace_checkpoint_v1(&self) -> Result<(), &'static str> {
        if self.profile_id != WORKSPACE_CHECKPOINT_V1_PROFILE {
            return Err("capability profile identity does not match the brokered profile");
        }
        if self.manifest_digest != WORKSPACE_CHECKPOINT_V1_MANIFEST_DIGEST {
            return Err("capability profile manifest digest does not match the brokered profile");
        }
        Ok(())
    }

    /// Verifies that the requested identity is the closed private v5 profile.
    pub fn verify_workspace_write_v1(&self) -> Result<(), &'static str> {
        if self.profile_id != WORKSPACE_WRITE_V1_PROFILE {
            return Err("capability profile identity does not match the brokered profile");
        }
        if self.manifest_digest != WORKSPACE_WRITE_V1_MANIFEST_DIGEST {
            return Err("capability profile manifest digest does not match the brokered profile");
        }
        Ok(())
    }
}

/// Verifies the actual Core inventory against the private v3 task-only profile.
pub fn verify_task_only_runtime_tools(actual: &[String]) -> Result<(), &'static str> {
    if actual
        .iter()
        .map(String::as_str)
        .eq(TASK_ONLY_V1_RUNTIME_TOOLS.iter().copied())
    {
        Ok(())
    } else {
        Err("Runtime tool inventory does not match the brokered capability profile")
    }
}

/// Verifies the actual Core inventory against the private v4 checkpoint profile.
pub fn verify_workspace_checkpoint_runtime_tools(actual: &[String]) -> Result<(), &'static str> {
    if actual
        .iter()
        .map(String::as_str)
        .eq(WORKSPACE_CHECKPOINT_V1_RUNTIME_TOOLS.iter().copied())
    {
        Ok(())
    } else {
        Err("Runtime tool inventory does not match the brokered capability profile")
    }
}

/// Verifies the actual Core inventory against the private v5 workspace-write profile.
pub fn verify_workspace_write_runtime_tools(actual: &[String]) -> Result<(), &'static str> {
    if actual
        .iter()
        .map(String::as_str)
        .eq(WORKSPACE_WRITE_V1_RUNTIME_TOOLS.iter().copied())
    {
        Ok(())
    } else {
        Err("Runtime tool inventory does not match the brokered capability profile")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_only_identity_and_inventory_are_exact() {
        let identity = BrokeredCapabilityProfileIdentity::task_only_v1();
        assert_eq!(identity.profile_id, "task-only-v1");
        assert_eq!(identity.manifest_digest, TASK_ONLY_V1_MANIFEST_DIGEST);
        assert_eq!(identity.verify_task_only_v1(), Ok(()));
        assert_eq!(
            verify_task_only_runtime_tools(&["ask_user_question".to_owned()]),
            Ok(())
        );
    }

    #[test]
    fn task_only_identity_and_inventory_reject_drift() {
        let mut identity = BrokeredCapabilityProfileIdentity::task_only_v1();
        identity.manifest_digest = "0".repeat(64);
        assert!(identity.verify_task_only_v1().is_err());
        assert!(verify_task_only_runtime_tools(&[]).is_err());
        assert!(verify_task_only_runtime_tools(&[
            "ask_user_question".to_owned(),
            "shell".to_owned(),
        ])
        .is_err());
    }

    #[test]
    fn checkpoint_identity_and_inventory_are_exact() {
        let identity = BrokeredCapabilityProfileIdentity::workspace_checkpoint_v1();
        assert_eq!(identity.profile_id, "workspace-checkpoint-v1");
        assert_eq!(
            identity.manifest_digest,
            WORKSPACE_CHECKPOINT_V1_MANIFEST_DIGEST
        );
        assert_eq!(identity.verify_workspace_checkpoint_v1(), Ok(()));
        assert_eq!(
            verify_workspace_checkpoint_runtime_tools(&[
                "ask_user_question".to_owned(),
                "workspace_checkpoint_create".to_owned(),
            ]),
            Ok(())
        );
    }

    #[test]
    fn checkpoint_identity_and_inventory_reject_drift() {
        let task_only = BrokeredCapabilityProfileIdentity::task_only_v1();
        assert!(task_only.verify_workspace_checkpoint_v1().is_err());
        assert!(verify_workspace_checkpoint_runtime_tools(&[
            "workspace_checkpoint_create".to_owned(),
            "ask_user_question".to_owned(),
        ])
        .is_err());
        assert!(verify_workspace_checkpoint_runtime_tools(&[
            "ask_user_question".to_owned(),
            "workspace_checkpoint_create".to_owned(),
            "shell".to_owned(),
        ])
        .is_err());
    }

    #[test]
    fn workspace_write_identity_and_inventory_are_exact() {
        let identity = BrokeredCapabilityProfileIdentity::workspace_write_v1();
        assert_eq!(identity.profile_id, "workspace-write-v1");
        assert_eq!(identity.manifest_digest, WORKSPACE_WRITE_V1_MANIFEST_DIGEST);
        assert_eq!(identity.verify_workspace_write_v1(), Ok(()));
        assert_eq!(
            verify_workspace_write_runtime_tools(&[
                "ask_user_question".to_owned(),
                "write_file".to_owned(),
            ]),
            Ok(())
        );
    }

    #[test]
    fn workspace_write_identity_and_inventory_reject_drift() {
        let mut identity = BrokeredCapabilityProfileIdentity::workspace_write_v1();
        identity.manifest_digest = "0".repeat(64);
        assert!(identity.verify_workspace_write_v1().is_err());
        assert!(verify_workspace_write_runtime_tools(&[
            "write_file".to_owned(),
            "ask_user_question".to_owned(),
        ])
        .is_err());
        assert!(verify_workspace_write_runtime_tools(&[
            "ask_user_question".to_owned(),
            "write_file".to_owned(),
            "edit".to_owned(),
        ])
        .is_err());
    }
}
