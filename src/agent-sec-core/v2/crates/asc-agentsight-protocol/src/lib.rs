//! `AgentSight` file-protection wire types shared with the file-policy intent.

#![forbid(unsafe_code)]

pub use asc_file_policy::{
    FILE_PROTECTION_POLICY_VERSION, FileProtectionExclusion, FileProtectionExclusionKind,
    FileProtectionPolicy, FileProtectionTarget, FileProtectionTargetKind,
    FileProtectionTopologyMode, FileProtections,
};

/// Media type of the `AgentSight` version-1 file-protection payload.
pub const FILE_PROTECTION_POLICY_MEDIA_TYPE: &str =
    "application/vnd.agentsight.file-protection.v1+json";
