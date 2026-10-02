//! Policy enforcement gates on the FUSE write path.
//!
//! Bundles the helpers that consult [`crate::security`] before letting
//! a mutating callback touch the underlying filesystem:
//!
//! * `evaluate_trusted_writer` / `policy_check` — pure consults.
//! * `enforce_skill_meta` — the `.skill-meta/**` mutation gate, with
//!   the trusted-writer bypass and its audit decoration.
//! * `lifecycle_reservation` / `enforce_lifecycle_reservation` —
//!   reject mutations under the reserved lifecycle namespaces.
//! * `check_physical_access_result` — POSIX `access(2)` permission
//!   check against the underlying inode (used by the `access` FUSE
//!   callback).

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use fuser::Request;

use super::SkillFs;
use crate::path::PathType;
use crate::security::{
    self, PathPolicy, PolicyDecision, SkillEvent, SkillEventAction, SkillEventKind,
    TrustedWriterDecision, evaluate_trusted_writer, is_skill_meta_path,
    lifecycle::{LifecycleNameClass, classify_skill_name as classify_lifecycle_name},
};
use crate::sys::errno;

impl SkillFs {
    /// Evaluate the trusted-writer gate for `req`. The result is
    /// pure — no audit emission. Call sites are responsible for
    /// folding the [`TrustedWriterDecision`] into their event detail
    /// string and returning the appropriate errno.
    pub(super) fn evaluate_trusted_writer(&self, req: &Request) -> TrustedWriterDecision {
        evaluate_trusted_writer(
            &self.trusted_writer,
            req.pid(),
            self.trusted_writer_identity.as_ref(),
        )
    }

    /// Run the configured Skill Security policy against `ctx`.
    pub(super) fn policy_check(&self, ctx: &PathPolicy<'_>) -> PolicyDecision {
        self.policy.check_path(ctx)
    }

    /// Centralized `.skill-meta` mutation gate.
    ///
    /// Looks at the parsed path and the operation kind, asks the configured
    /// policy whether the mutation is allowed, and on `Deny` emits a
    /// `PolicyDenied` event and returns `Some(errno)` so the caller can
    /// short-circuit. `Allow` (or paths the policy cannot reason about)
    /// returns `None`.
    pub(super) fn enforce_skill_meta(
        &self,
        path_type: &PathType,
        operation: SkillEventKind,
        req: &Request,
        detail: Option<String>,
    ) -> Option<i32> {
        let nested_id;
        let (skill_name, relative_path) = match path_type {
            PathType::Passthrough {
                skill_name,
                relative_path,
            }
            | PathType::InboxPassthrough {
                skill_name,
                relative_path,
            } => (skill_name.as_str(), relative_path.as_path()),
            PathType::NestedPassthrough {
                category,
                skill_name,
                relative_path,
            } => {
                nested_id = Self::hermes_skill_id(category, skill_name);
                (nested_id.as_str(), relative_path.as_path())
            }
            _ => return None,
        };

        let ctx = PathPolicy::new(operation)
            .with_skill_name(Some(skill_name))
            .with_relative_path(Some(relative_path));
        match self.policy_check(&ctx) {
            PolicyDecision::Allow => None,
            PolicyDecision::Deny { errno, reason } => {
                // trusted-writer bypass. Strictly scoped to
                // `.skill-meta/**`: only consulted when the deny actually
                // covers a `.skill-meta/**` path, so a future policy that
                // denies for a different reason is unaffected. The bypass
                // is observable via an audit `PolicyDecision` (allowed)
                // record carrying `trusted_writer=<name>` in `detail`.
                let trusted_writer_bypass_allowed = !matches!(
                    operation,
                    SkillEventKind::SymlinkAttempt | SkillEventKind::HardlinkAttempt
                );
                if security::is_skill_meta_path(relative_path) && trusted_writer_bypass_allowed {
                    let twd = self.evaluate_trusted_writer(req);
                    if twd.is_allowed() {
                        let identity_detail = match &twd {
                            TrustedWriterDecision::AllowedByExecutable { path, file_id } => {
                                format!("trusted_writer_exe={} {}", path.display(), file_id)
                            }
                            TrustedWriterDecision::AllowedByName { name } => {
                                format!("trusted_writer={}", name)
                            }
                            _ => unreachable!(),
                        };
                        let mut allow_detail = format!(
                            "op={:?} reason={} class={} {}",
                            operation,
                            reason,
                            twd.audit_label(),
                            identity_detail
                        );
                        if let Some(extra) = detail.as_ref() {
                            allow_detail.push(' ');
                            allow_detail.push_str(extra);
                        }
                        let event = SkillEvent::new(SkillEventKind::PolicyDecision)
                            .with_skill_name(skill_name)
                            .with_relative_path(relative_path)
                            .with_action(SkillEventAction::Allowed)
                            .with_caller(req.uid(), req.gid())
                            .with_detail(allow_detail);
                        self.emit_event(event);
                        return None;
                    }
                    let is_configured_deny = !matches!(
                        twd,
                        TrustedWriterDecision::Disabled
                            | TrustedWriterDecision::AllowedByName { .. }
                            | TrustedWriterDecision::AllowedByExecutable { .. }
                    );
                    if is_configured_deny {
                        let mut deny_detail = match &twd {
                            TrustedWriterDecision::DeniedNameMismatch { actual, expected } => {
                                format!(
                                    "op={:?} reason={} class={} trusted_writer_actual={} trusted_writer_expected={}",
                                    operation,
                                    reason,
                                    twd.audit_label(),
                                    actual,
                                    expected
                                )
                            }
                            TrustedWriterDecision::DeniedStarttimeMismatch {
                                pid,
                                pinned,
                                actual,
                            } => {
                                format!(
                                    "op={:?} reason={} class={} pid={} pinned_starttime={} actual_starttime={}",
                                    operation,
                                    reason,
                                    twd.audit_label(),
                                    pid,
                                    pinned,
                                    actual
                                )
                            }
                            TrustedWriterDecision::DeniedExecutableMismatch {
                                expected,
                                actual,
                            } => {
                                format!(
                                    "op={:?} reason={} class={} exe_expected={} exe_actual={}",
                                    operation,
                                    reason,
                                    twd.audit_label(),
                                    expected.display(),
                                    actual.display()
                                )
                            }
                            TrustedWriterDecision::DeniedExecutableFileIdMismatch {
                                expected,
                                actual,
                            } => {
                                format!(
                                    "op={:?} reason={} class={} expected_file_id=({}) actual_file_id=({})",
                                    operation,
                                    reason,
                                    twd.audit_label(),
                                    expected,
                                    actual
                                )
                            }
                            _ => format!(
                                "op={:?} reason={} class={}",
                                operation,
                                reason,
                                twd.audit_label()
                            ),
                        };
                        if let Some(extra) = detail.as_ref() {
                            deny_detail.push(' ');
                            deny_detail.push_str(extra);
                        }
                        let event = SkillEvent::new(SkillEventKind::PolicyDenied)
                            .with_skill_name(skill_name)
                            .with_relative_path(relative_path)
                            .with_action(SkillEventAction::Rejected)
                            .with_errno(errno)
                            .with_caller(req.uid(), req.gid())
                            .with_detail(deny_detail);
                        self.emit_event(event);
                        self.metric_policy_denied();
                        return Some(errno);
                    }
                }
                let mut event = SkillEvent::new(SkillEventKind::PolicyDenied)
                    .with_skill_name(skill_name)
                    .with_relative_path(relative_path)
                    .with_action(SkillEventAction::Rejected)
                    .with_errno(errno)
                    .with_caller(req.uid(), req.gid())
                    .with_detail(format!("op={:?} reason={}", operation, reason));
                if let Some(extra) = detail {
                    event = event
                        .with_detail(format!("op={:?} reason={} {}", operation, reason, extra));
                }
                self.emit_event(event);
                self.metric_policy_denied();
                Some(errno)
            }
        }
    }

    /// Return the lifecycle namespace name reserved by Package S3 when the
    /// parsed path's top-level skill-name segment matches one of
    /// `.staging`, `.certified`, `.quarantine`, or `.archive`. Otherwise
    /// returns `None`.
    ///
    /// The check is purely lexical and applies to `SkillDir`, `SkillMd`,
    /// and `Passthrough` paths — i.e. any FUSE path that lives below the
    /// reserved top-level segment. Root, SkillsDir, and Invalid never have
    /// a skill-name component and always return `None`.
    pub(super) fn lifecycle_reservation(path_type: &PathType) -> Option<&'static str> {
        let skill_name = match path_type {
            PathType::SkillDir { skill_name }
            | PathType::SkillMd { skill_name }
            | PathType::Passthrough { skill_name, .. }
            | PathType::InboxSkillDir { skill_name }
            | PathType::InboxPassthrough { skill_name, .. } => skill_name.as_str(),
            _ => return None,
        };
        match classify_lifecycle_name(skill_name) {
            LifecycleNameClass::Reserved(canonical) => Some(canonical),
            LifecycleNameClass::Ordinary => None,
        }
    }

    /// Centralized lifecycle namespace reservation gate (Package S3).
    ///
    /// When the parsed path resolves to a reserved lifecycle namespace
    /// (`.staging`, `.certified`, `.quarantine`, `.archive`), emits a
    /// `PolicyDenied` audit event with `EACCES` and returns
    /// `Some(libc::EACCES)` so callers can short-circuit before touching
    /// the underlying filesystem. Returns `None` for ordinary paths.
    ///
    /// The reservation is enforced for **mutating** operations only
    /// (`Create`, `Delete`, `Rename`, `Write`, `Metadata`,
    /// `SymlinkAttempt`, `HardlinkAttempt`); non-mutating operations
    /// observe the boundary through hidden lookup/readdir at virtual-view
    /// layers. Phase 1 errno semantics for ordinary paths are preserved.
    pub(super) fn enforce_lifecycle_reservation(
        &self,
        path_type: &PathType,
        operation: SkillEventKind,
        req: &Request,
        detail: Option<String>,
    ) -> Option<i32> {
        let canonical = Self::lifecycle_reservation(path_type)?;
        let errno = libc::EACCES;
        let reason = "lifecycle namespace is reserved";
        let (skill_name, relative_path) = match path_type {
            PathType::Passthrough {
                skill_name,
                relative_path,
            }
            | PathType::InboxPassthrough {
                skill_name,
                relative_path,
            } => (skill_name.clone(), Some(relative_path.clone())),
            PathType::SkillMd { skill_name } => {
                (skill_name.clone(), Some(PathBuf::from("SKILL.md")))
            }
            PathType::SkillDir { skill_name } | PathType::InboxSkillDir { skill_name } => {
                (skill_name.clone(), None)
            }
            _ => return None,
        };
        let mut event = SkillEvent::new(SkillEventKind::PolicyDenied)
            .with_skill_name(skill_name)
            .with_action(SkillEventAction::Rejected)
            .with_errno(errno)
            .with_caller(req.uid(), req.gid());
        if let Some(rel) = relative_path {
            event = event.with_relative_path(rel);
        }
        let base_detail = format!(
            "op={:?} reason={} lifecycle={}",
            operation, reason, canonical
        );
        let final_detail = match detail {
            Some(extra) => format!("{} {}", base_detail, extra),
            None => base_detail,
        };
        event = event.with_detail(final_detail);
        self.emit_event(event);
        self.metric_policy_denied();
        Some(errno)
    }

    /// Check physical file access permissions.
    ///
    /// Returns 0 on success, or an errno value on failure.
    pub(super) fn check_physical_access_result(
        &self,
        path: &Path,
        mask: i32,
        req: &Request,
    ) -> i32 {
        if mask == libc::F_OK {
            return match std::fs::metadata(path) {
                Ok(_) => 0,
                Err(e) => errno(&e),
            };
        }
        let metadata = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) => return errno(&e),
        };
        let mode = metadata.mode();
        let file_uid = metadata.uid();
        let file_gid = metadata.gid();
        let caller_uid = req.uid();
        let caller_gid = req.gid();

        if caller_uid == 0 {
            if (mask & libc::X_OK) != 0 && (mode & 0o111) == 0 {
                return libc::EACCES;
            }
            return 0;
        }

        // NOTE: FUSE protocol only provides the caller's primary gid via req.gid().
        // Supplementary group membership is not available in the FUSE request,
        // which may cause false negatives when access is granted via supplementary groups.
        // This is a known limitation of the FUSE protocol.
        let perm_bits = if caller_uid == file_uid {
            (mode >> 6) & 0o7
        } else if caller_gid == file_gid {
            (mode >> 3) & 0o7
        } else {
            mode & 0o7
        };

        if (mask & libc::R_OK) != 0 && (perm_bits & 0o4) == 0 {
            return libc::EACCES;
        }
        if (mask & libc::W_OK) != 0 && (perm_bits & 0o2) == 0 {
            return libc::EACCES;
        }
        if (mask & libc::X_OK) != 0 && (perm_bits & 0o1) == 0 {
            return libc::EACCES;
        }
        0
    }

    /// Trusted `.skill-meta` read-path gate.
    ///
    /// Returns:
    /// * `None` — path is not `.skill-meta/**`; caller proceeds normally.
    /// * `Some(true)` — path is `.skill-meta/**` and caller is trusted;
    ///   caller should route to **live source** via `skill_physical_dir`.
    /// * `Some(false)` — path is `.skill-meta/**` and caller is
    ///   untrusted; caller should deny/hide (ENOENT).
    pub(super) fn is_trusted_skill_meta_access(
        &self,
        path_type: &PathType,
        req: &Request,
    ) -> Option<bool> {
        let relative_path = match path_type {
            PathType::Passthrough { relative_path, .. }
            | PathType::InboxPassthrough { relative_path, .. }
            | PathType::NestedPassthrough { relative_path, .. } => relative_path.as_path(),
            _ => return None,
        };
        if !is_skill_meta_path(relative_path) {
            return None;
        }
        Some(self.evaluate_trusted_writer(req).is_allowed())
    }

    /// Whether `.skill-meta` should appear in a `SkillDir` listing.
    pub(super) fn should_show_skill_meta_in_listing(
        &self,
        skill_name: &str,
        req: &Request,
    ) -> bool {
        let probe = PathType::Passthrough {
            skill_name: skill_name.to_string(),
            relative_path: std::path::PathBuf::from(crate::security::SKILL_META_DIR),
        };
        self.is_trusted_skill_meta_access(&probe, req) == Some(true)
    }

    /// Whether `.skill-meta` should appear in a `NestedSkillDir` listing.
    pub(super) fn should_show_skill_meta_in_nested_listing(
        &self,
        category: &str,
        skill_name: &str,
        req: &Request,
    ) -> bool {
        let probe = PathType::NestedPassthrough {
            category: category.to_string(),
            skill_name: skill_name.to_string(),
            relative_path: std::path::PathBuf::from(crate::security::SKILL_META_DIR),
        };
        self.is_trusted_skill_meta_access(&probe, req) == Some(true)
    }
}
