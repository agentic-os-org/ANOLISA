//! Audit event emission and refresh-controller dispatch.
//!
//! Centralizes the helpers FUSE callbacks call at success/failure
//! branches: `emit_event` / `emit_op_event` / `emit_xattr_event` for
//! `SkillEvent` audit records, `observe_mutation` /
//! `inbox_observe_install_complete` for the refresh controller,
//! and `send_sync` for the background store-sync worker.

use std::path::{Path, PathBuf};

use fuser::Request;

use super::SkillFs;
use crate::path::PathType;
use crate::security::{
    MutationKind, SkillEvent, SkillEventAction, SkillEventKind, inbox::is_install_complete_path,
};
use crate::sync::SyncEvent;

impl SkillFs {
    /// Best-effort dispatch to the refresh controller. Skill-discover
    /// and virtual paths are filtered before this is called; the
    /// controller still applies its own filters (`.skill-meta`,
    /// lifecycle reserved roots) so the FUSE side does not need to
    /// know about ledger-internal paths. Failures are silently
    /// dropped; refresh must never propagate back to a FUSE
    /// callback's reply path.
    pub(super) fn observe_mutation(
        &self,
        skill_name: &str,
        relative_path: Option<&Path>,
        kind: MutationKind,
    ) {
        // I2/H3: staging roots bypass normal refresh/notify.
        if let Some(ref matcher) = self.staging_matcher {
            if matcher.is_staging_root(skill_name) {
                return;
            }
            // H3: Hermes nested ID — check the leaf component.
            if let Some(leaf) = skill_name.split('/').next_back() {
                if leaf != skill_name && matcher.is_staging_root(leaf) {
                    return;
                }
            }
        }
        // Pending install: if a pending_install_controller is attached,
        // check the skill's activation state.
        if let Some(ref pending_ctrl) = self.pending_install_controller {
            let has_activation = self
                .active_resolver
                .as_ref()
                .is_some_and(|r| r.get(skill_name).is_some());
            if has_activation {
                pending_ctrl.clear_if_activated(skill_name);
            } else if pending_ctrl.observe_mutation(skill_name, relative_path, kind) {
                return;
            }
        }
        if let Some(ctrl) = self.refresh_controller.as_ref() {
            ctrl.observe_mutation(skill_name, relative_path, kind);
        }
        if let Some(ctrl) = self.notify_controller.as_ref() {
            ctrl.observe_mutation(skill_name, relative_path, kind);
        }
        if let Some(ctrl) = self.quiet_timeout_controller.as_ref() {
            ctrl.observe_skill_mutation(skill_name, relative_path, kind);
        }
    }

    /// L1 inbox refresh trigger. Inbox writes never run scan/resolve
    /// per-chunk: doing so would re-run the External Decision pipeline
    /// dozens of times during a single multi-file install. The
    /// installer instead writes the
    /// [`crate::security::INSTALL_COMPLETE_SENTINEL`] file under
    /// `<inbox>/<skill>/`, and only that sentinel enqueues a single
    /// debounced `scan -> resolve` for the owning skill. This helper
    /// is the single call site for inbox-side mutating callbacks; the
    /// `relative_path` argument is the path inside the skill directory
    /// the callback is mutating.
    pub(super) fn inbox_observe_install_complete(
        &self,
        skill_name: &str,
        relative_path: &Path,
        kind: MutationKind,
    ) {
        if !is_install_complete_path(relative_path) {
            return;
        }
        if let Some(ctrl) = self.refresh_controller.as_ref() {
            ctrl.observe_mutation(skill_name, Some(relative_path), kind);
        }
        if let Some(ctrl) = self.notify_controller.as_ref() {
            ctrl.observe_mutation(skill_name, Some(relative_path), kind);
        }
    }

    /// Best-effort event emission. The result is intentionally discarded —
    /// sinks are expected to be non-blocking, but SkillFS does not retry or
    /// surface a misbehaving sink either way (see
    /// [`crate::security::SkillEventSink`]).
    pub(super) fn emit_event(&self, event: SkillEvent) {
        self.event_sink.emit(&event);
    }

    /// Build and emit a normalized event for a FUSE operation given the
    /// parsed `path_type` and a known outcome. Centralized so individual
    /// callbacks add a single line at the success/failure branches rather
    /// than reconstructing skill name + relative path inline each time.
    ///
    /// This helper always allocates the [`SkillEvent`] (including any
    /// `PathBuf` clone) before invoking the sink — the default
    /// [`crate::security::NoopEventSink`] then drops it cheaply. A future
    /// optimization could add an `events_enabled` fast path; doing so today
    /// would require reaching into the trait object and is not worth the
    /// complexity while FUSE callback rates are dominated by I/O work.
    pub(super) fn emit_op_event(
        &self,
        req: &Request,
        path_type: &PathType,
        kind: SkillEventKind,
        action: SkillEventAction,
        errno_value: Option<i32>,
        bytes: Option<u64>,
    ) {
        self.emit_op_event_with_detail(req, path_type, kind, action, errno_value, bytes, None);
    }

    /// Emit a normalized operation event with an optional content-free detail.
    /// Existing call sites use [`Self::emit_op_event`]; only operations with
    /// meaningful context need this variant.
    pub(super) fn emit_op_event_with_detail(
        &self,
        req: &Request,
        path_type: &PathType,
        kind: SkillEventKind,
        action: SkillEventAction,
        errno_value: Option<i32>,
        bytes: Option<u64>,
        detail: Option<String>,
    ) {
        let (skill_name, relative_path) = match path_type {
            PathType::Passthrough {
                skill_name,
                relative_path,
            }
            | PathType::InboxPassthrough {
                skill_name,
                relative_path,
            } => (Some(skill_name.clone()), Some(relative_path.clone())),
            PathType::SkillMd { skill_name } => {
                (Some(skill_name.clone()), Some(PathBuf::from("SKILL.md")))
            }
            PathType::SkillDir { skill_name } | PathType::InboxSkillDir { skill_name } => {
                (Some(skill_name.clone()), None)
            }
            PathType::NestedSkillDir {
                category,
                skill_name,
            } => (Some(Self::hermes_skill_id(category, skill_name)), None),
            PathType::NestedSkillMd {
                category,
                skill_name,
            } => (
                Some(Self::hermes_skill_id(category, skill_name)),
                Some(PathBuf::from("SKILL.md")),
            ),
            PathType::NestedPassthrough {
                category,
                skill_name,
                relative_path,
            } => (
                Some(Self::hermes_skill_id(category, skill_name)),
                Some(relative_path.clone()),
            ),
            _ => (None, None),
        };
        let mut event = SkillEvent::new(kind)
            .with_optional_skill_name(skill_name)
            .with_optional_relative_path(relative_path)
            .with_action(action)
            .with_caller(req.uid(), req.gid());
        if let Some(e) = errno_value {
            event = event.with_errno(e);
        }
        if let Some(b) = bytes {
            event = event.with_bytes(b);
        }
        if let Some(detail) = detail {
            event = event.with_detail(detail);
        }
        self.emit_event(event);
    }

    /// Stable, content-free audit context for an enabled OS adapter.
    pub(super) fn os_adapter_open_detail(&self) -> Option<String> {
        self.transform_pipeline
            .os_adapter_metadata()
            .map(|metadata| {
                format!(
                    "transform=os_adapter target_os={} rule_digest={}",
                    metadata.target_os(),
                    metadata.rule_digest()
                )
            })
    }

    /// Emit a normalized `Metadata` event for an xattr mutation (`setxattr`
    /// or `removexattr`). The xattr verb (`set` / `remove`) and the xattr
    /// name are folded into the existing `detail` string so the JSONL audit
    /// shape stays backwards compatible.
    ///
    /// `class` is an optional snake_case label appended to the audit
    /// `detail` string when present. It lets rejection branches name *why*
    /// the request was refused (e.g. `virtual_xattr_path`,
    /// `unsupported_xattr_namespace`) without inventing new JSONL fields.
    pub(super) fn emit_xattr_event(
        &self,
        req: &Request,
        path_type: &PathType,
        verb: &str,
        name: &std::ffi::OsStr,
        action: SkillEventAction,
        errno_value: Option<i32>,
        class: Option<&str>,
    ) {
        let (skill_name, relative_path) = match path_type {
            PathType::Passthrough {
                skill_name,
                relative_path,
            } => (Some(skill_name.clone()), Some(relative_path.clone())),
            PathType::SkillMd { skill_name } => {
                (Some(skill_name.clone()), Some(PathBuf::from("SKILL.md")))
            }
            PathType::SkillDir { skill_name } => (Some(skill_name.clone()), None),
            PathType::NestedSkillDir {
                category,
                skill_name,
            } => (Some(Self::hermes_skill_id(category, skill_name)), None),
            PathType::NestedSkillMd {
                category,
                skill_name,
            } => (
                Some(Self::hermes_skill_id(category, skill_name)),
                Some(PathBuf::from("SKILL.md")),
            ),
            PathType::NestedPassthrough {
                category,
                skill_name,
                relative_path,
            } => (
                Some(Self::hermes_skill_id(category, skill_name)),
                Some(relative_path.clone()),
            ),
            _ => (None, None),
        };
        let display_name = name.to_string_lossy();
        let detail = match class {
            Some(c) => format!("xattr={} name={} class={}", verb, display_name, c),
            None => format!("xattr={} name={}", verb, display_name),
        };
        let mut event = SkillEvent::new(SkillEventKind::Metadata)
            .with_optional_skill_name(skill_name)
            .with_optional_relative_path(relative_path)
            .with_action(action)
            .with_caller(req.uid(), req.gid())
            .with_detail(detail);
        if let Some(e) = errno_value {
            event = event.with_errno(e);
        }
        self.emit_event(event);
    }

    /// Send a sync event to the background worker (non-blocking).
    pub(super) fn send_sync(&self, event: SyncEvent) {
        if let Some(ref tx) = self.sync_tx {
            let _ = tx.send(event);
        }
    }
}
