//! Skill Security policy seam.
//!
//! S0 introduced a permissive default policy and a trait that future
//! packages can implement to deny operations. S1 adds
//! [`SkillMetaProtectionPolicy`] — the first real policy implementation —
//! which denies mutating operations under `.skill-meta/**` while leaving
//! reads alone.

use std::path::Path;

use super::event::SkillEventKind;
use super::path::is_skill_meta_path;

/// Outcome of a policy check. `Allow` is the only variant emitted in S0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    /// Operation may proceed.
    Allow,
    /// Operation must be denied with the given errno. The reason is purely
    /// informational and may be logged or surfaced via events.
    Deny { errno: i32, reason: String },
}

impl PolicyDecision {
    pub fn allow() -> Self {
        Self::Allow
    }

    pub fn deny(errno: i32, reason: impl Into<String>) -> Self {
        Self::Deny {
            errno,
            reason: reason.into(),
        }
    }

    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// Borrowed context describing the path a policy check is about. New fields
/// may be added in later packages (caller pid/exe, lifecycle phase, etc.).
#[derive(Debug, Clone)]
pub struct PathPolicy<'a> {
    /// Skill that owns the path, if known.
    pub skill_name: Option<&'a str>,
    /// Path relative to the skill directory (or `SKILL.md`), if known.
    pub relative_path: Option<&'a Path>,
    /// Operation that is about to run.
    pub operation: SkillEventKind,
}

impl<'a> PathPolicy<'a> {
    pub fn new(operation: SkillEventKind) -> Self {
        Self {
            skill_name: None,
            relative_path: None,
            operation,
        }
    }

    pub fn with_skill_name(mut self, skill_name: Option<&'a str>) -> Self {
        self.skill_name = skill_name;
        self
    }

    pub fn with_relative_path(mut self, relative_path: Option<&'a Path>) -> Self {
        self.relative_path = relative_path;
        self
    }
}

/// Trait implemented by Skill Security policies. Default method bodies
/// allow everything so `impl SecurityPolicy for MyPolicy {}` produces a
/// permissive policy unless individual checks are overridden.
pub trait SecurityPolicy: Send + Sync + 'static {
    /// Decide whether `ctx` may proceed. Default: always `Allow`.
    fn check_path(&self, ctx: &PathPolicy<'_>) -> PolicyDecision {
        let _ = ctx;
        PolicyDecision::Allow
    }
}

/// Permissive policy that allows every operation.
///
/// Retained as an opt-in for tests, embedding scenarios, and any future
/// caller that wants to disable Skill Security checks entirely. Used to be
/// the default in S0; S1 swapped the default to
/// [`SkillMetaProtectionPolicy`].
#[derive(Debug, Default, Clone, Copy)]
pub struct PermissivePolicy;

impl SecurityPolicy for PermissivePolicy {}

/// Returns `true` for every [`SkillEventKind`] that mutates filesystem state.
///
/// Reads (`Open` for read, `Read`, `Readlink`) and pure metadata observation
/// are **not** mutating. Open is intentionally not classified as mutating
/// here because the open's exact intent (read vs. write) is decided at the
/// FUSE callback layer with full access to the open flags; the policy
/// receives a more specific event kind there.
pub fn is_mutating_kind(kind: SkillEventKind) -> bool {
    matches!(
        kind,
        SkillEventKind::Write
            | SkillEventKind::Create
            | SkillEventKind::Delete
            | SkillEventKind::Rename
            | SkillEventKind::Metadata
            | SkillEventKind::SymlinkAttempt
            | SkillEventKind::HardlinkAttempt
    )
}

/// Default Skill Security policy. Denies mutating operations targeted at
/// `.skill-meta/**` paths under any skill, while allowing every other
/// operation through.
///
/// Behavior:
/// * Mutations on `.skill-meta` itself or anything beneath it → `Deny`
///   with `EACCES` and a short reason string.
/// * Reads on `.skill-meta/**` → `Allow` (the FUSE layer still consults the
///   underlying physical permissions afterwards).
/// * Anything outside `.skill-meta` → `Allow`.
///
/// The policy is purely lexical and consults
/// [`super::path::is_skill_meta_path`]; it performs no I/O. Trusted-writer
/// support, audit JSONL, capability gates, and quarantine semantics are
/// out of scope for S1.
#[derive(Debug, Default, Clone, Copy)]
pub struct SkillMetaProtectionPolicy;

impl SecurityPolicy for SkillMetaProtectionPolicy {
    fn check_path(&self, ctx: &PathPolicy<'_>) -> PolicyDecision {
        // The `skill-discover` namespace is fully virtual and already
        // read-only. Its FUSE-level rejections (EROFS) take precedence; the
        // `.skill-meta` policy must not layer EACCES on top, so its virtual
        // semantics stay untouched even if a probe lands on a path string
        // that happens to look like `.skill-meta/**`.
        if ctx.skill_name == Some("skill-discover") {
            return PolicyDecision::Allow;
        }
        let Some(rel) = ctx.relative_path else {
            return PolicyDecision::Allow;
        };
        if !is_skill_meta_path(rel) {
            return PolicyDecision::Allow;
        }
        if !is_mutating_kind(ctx.operation) {
            return PolicyDecision::Allow;
        }
        PolicyDecision::deny(libc::EACCES, ".skill-meta is read-only")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_meta_policy_denies_mutating_kinds_under_meta() {
        let policy = SkillMetaProtectionPolicy;
        let meta_paths = [
            Path::new(".skill-meta"),
            Path::new(".skill-meta/manifest.json"),
            Path::new(".skill-meta/signatures/root.json"),
        ];
        let mutating = [
            SkillEventKind::Write,
            SkillEventKind::Create,
            SkillEventKind::Delete,
            SkillEventKind::Rename,
            SkillEventKind::Metadata,
            SkillEventKind::SymlinkAttempt,
            SkillEventKind::HardlinkAttempt,
        ];
        for path in &meta_paths {
            for kind in mutating {
                let ctx = PathPolicy::new(kind)
                    .with_skill_name(Some("alpha"))
                    .with_relative_path(Some(path));
                let d = policy.check_path(&ctx);
                match d {
                    PolicyDecision::Deny { errno, .. } => {
                        assert_eq!(errno, libc::EACCES, "{:?} {:?}", kind, path)
                    }
                    PolicyDecision::Allow => panic!("expected Deny for {:?} on {:?}", kind, path),
                }
            }
        }
    }

    #[test]
    fn skill_meta_policy_allows_reads_under_meta() {
        let policy = SkillMetaProtectionPolicy;
        let meta = Path::new(".skill-meta/manifest.json");
        for kind in [
            SkillEventKind::Open,
            SkillEventKind::Read,
            SkillEventKind::Readlink,
        ] {
            let ctx = PathPolicy::new(kind)
                .with_skill_name(Some("alpha"))
                .with_relative_path(Some(meta));
            assert_eq!(policy.check_path(&ctx), PolicyDecision::Allow);
        }
    }

    #[test]
    fn skill_meta_policy_allows_non_meta_paths() {
        let policy = SkillMetaProtectionPolicy;
        let outside = Path::new("scripts/run.sh");
        for kind in [
            SkillEventKind::Write,
            SkillEventKind::Create,
            SkillEventKind::Delete,
            SkillEventKind::Rename,
            SkillEventKind::Metadata,
        ] {
            let ctx = PathPolicy::new(kind)
                .with_skill_name(Some("alpha"))
                .with_relative_path(Some(outside));
            assert_eq!(policy.check_path(&ctx), PolicyDecision::Allow);
        }
    }

    #[test]
    fn skill_meta_policy_allows_neighbour_names() {
        let policy = SkillMetaProtectionPolicy;
        for rel in [
            Path::new(".skill-meta2"),
            Path::new("docs/.skill-meta"),
            Path::new(".skill-met"),
        ] {
            let ctx = PathPolicy::new(SkillEventKind::Write)
                .with_skill_name(Some("alpha"))
                .with_relative_path(Some(rel));
            assert_eq!(policy.check_path(&ctx), PolicyDecision::Allow);
        }
    }

    #[test]
    fn skill_meta_policy_allows_when_relative_path_unknown() {
        // Without a relative path the policy cannot decide; defer to other
        // gates rather than over-block (e.g. virtual root operations that
        // already have their own EROFS handling).
        let policy = SkillMetaProtectionPolicy;
        let ctx = PathPolicy::new(SkillEventKind::Write).with_skill_name(Some("alpha"));
        assert_eq!(policy.check_path(&ctx), PolicyDecision::Allow);
    }

    #[test]
    fn skill_meta_policy_skips_skill_discover_namespace() {
        // skill-discover is a virtual read-only namespace. The FUSE layer
        // already rejects mutations there with EROFS; the `.skill-meta`
        // policy must not preempt that with EACCES, even for path strings
        // that look like `.skill-meta/**` under skill-discover.
        let policy = SkillMetaProtectionPolicy;
        let meta_paths = [
            Path::new(".skill-meta"),
            Path::new(".skill-meta/manifest.json"),
            Path::new(".skill-meta/signatures/root.json"),
        ];
        let mutating = [
            SkillEventKind::Write,
            SkillEventKind::Create,
            SkillEventKind::Delete,
            SkillEventKind::Rename,
            SkillEventKind::Metadata,
        ];
        for path in &meta_paths {
            for kind in mutating {
                let ctx = PathPolicy::new(kind)
                    .with_skill_name(Some("skill-discover"))
                    .with_relative_path(Some(path));
                assert_eq!(
                    policy.check_path(&ctx),
                    PolicyDecision::Allow,
                    "skill-discover/{:?} {:?} must not be denied by S1",
                    path,
                    kind,
                );
            }
        }

        // Sanity: a normal skill name is still protected on the same paths.
        for path in &meta_paths {
            let ctx = PathPolicy::new(SkillEventKind::Write)
                .with_skill_name(Some("alpha"))
                .with_relative_path(Some(path));
            match policy.check_path(&ctx) {
                PolicyDecision::Deny { errno, .. } => assert_eq!(errno, libc::EACCES),
                PolicyDecision::Allow => {
                    panic!("alpha/{:?} must remain protected", path)
                }
            }
        }
    }
}
