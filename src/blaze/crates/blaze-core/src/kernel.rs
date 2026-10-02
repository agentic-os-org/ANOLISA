// SPDX-License-Identifier: Apache-2.0
//! # Kernel Coordinator (v0.1: state-only stub)
//!
//! v0.1 maintains only a hook state machine; it does NOT push syscalls to `/proc/anolisa/...`.
//! Phase 3 will integrate real kernel interfaces (mm-template register, UFFD-WP activate, etc.).
//!
//! ## Concurrency model
//!
//! HookRegistry itself contains no synchronization primitives. Callers (blaze daemon)
//! must wrap it in `Mutex<HookRegistry>` or equivalent to ensure atomic activate/deactivate.
//!
//! Kernel hook registry.
//!
//! v0.1: state-only stub. The registry tracks per-hook activation but
//! does **not** push commands to `/proc/anolisa/...`; that lands in
//! Phase 3. Each hook kind is single-tenant: at most one instance can
//! own activation at a time (the per-hook mutex requirement).

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{BlazeError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HookKind {
    MmTemplate,
    UffdWp,
    CgroupHigh,
    EptLazyLoad,
}

impl HookKind {
    pub const fn as_str(&self) -> &'static str {
        match self {
            HookKind::MmTemplate => "mm-template",
            HookKind::UffdWp => "uffd-wp",
            HookKind::CgroupHigh => "cgroup-high",
            HookKind::EptLazyLoad => "ept-lazy-load",
        }
    }
}

impl fmt::Display for HookKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for HookKind {
    type Err = BlazeError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "mm-template" => Ok(HookKind::MmTemplate),
            "uffd-wp" => Ok(HookKind::UffdWp),
            "cgroup-high" => Ok(HookKind::CgroupHigh),
            "ept-lazy-load" => Ok(HookKind::EptLazyLoad),
            other => Err(BlazeError::HookError {
                hook_name: other.to_string(),
                msg: "unknown hook kind".into(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookState {
    Registered,
    Activated,
    Deactivated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookEntry {
    pub kind: HookKind,
    pub state: HookState,
    #[serde(default)]
    pub instance_id: Option<Uuid>,
    pub registered_at: DateTime<Utc>,
}

#[derive(Debug, Default)]
pub struct HookRegistry {
    hooks: HashMap<HookKind, HookEntry>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a hook so it can later be activated. Idempotent — if
    /// already registered we leave its current state alone.
    pub fn register(&mut self, kind: HookKind) -> Result<()> {
        self.hooks.entry(kind).or_insert(HookEntry {
            kind,
            state: HookState::Registered,
            instance_id: None,
            registered_at: Utc::now(),
        });
        tracing::info!(hook = %kind, "registered hook");
        Ok(())
    }

    pub fn unregister(&mut self, kind: HookKind) -> Result<()> {
        let entry = self.hooks.get(&kind).ok_or_else(|| BlazeError::HookError {
            hook_name: kind.to_string(),
            msg: "not registered".into(),
        })?;
        if entry.state == HookState::Activated {
            return Err(BlazeError::HookError {
                hook_name: kind.to_string(),
                msg: format!(
                    "cannot unregister while activated by instance {:?}",
                    entry.instance_id
                ),
            });
        }
        self.hooks.remove(&kind);
        tracing::info!(hook = %kind, "unregistered hook");
        Ok(())
    }

    /// Activate `kind` on behalf of `instance_id`. Per-hook mutex: if
    /// another instance currently owns it, returns
    /// [`BlazeError::HookError`].
    pub fn activate(&mut self, kind: HookKind, instance_id: Uuid) -> Result<()> {
        let entry = self
            .hooks
            .get_mut(&kind)
            .ok_or_else(|| BlazeError::HookError {
                hook_name: kind.to_string(),
                msg: "not registered".into(),
            })?;
        if entry.state == HookState::Activated {
            if entry.instance_id == Some(instance_id) {
                // already ours, idempotent
                return Ok(());
            }
            return Err(BlazeError::HookError {
                hook_name: kind.to_string(),
                msg: format!("already activated by instance {:?}", entry.instance_id),
            });
        }
        entry.state = HookState::Activated;
        entry.instance_id = Some(instance_id);
        tracing::info!(hook = %kind, instance = %instance_id, "activated hook");
        Ok(())
    }

    /// Deactivate a hook held by `instance_id`. Returns an error when
    /// the hook is held by a different instance — preventing one
    /// sandbox from yanking another's hook out from under it.
    pub fn deactivate(&mut self, kind: HookKind, instance_id: Uuid) -> Result<()> {
        let entry = self
            .hooks
            .get_mut(&kind)
            .ok_or_else(|| BlazeError::HookError {
                hook_name: kind.to_string(),
                msg: "not registered".into(),
            })?;
        if entry.state != HookState::Activated {
            return Err(BlazeError::HookError {
                hook_name: kind.to_string(),
                msg: "not currently activated".into(),
            });
        }
        if entry.instance_id != Some(instance_id) {
            return Err(BlazeError::HookError {
                hook_name: kind.to_string(),
                msg: format!(
                    "owned by instance {:?}, refusing to deactivate from {instance_id}",
                    entry.instance_id
                ),
            });
        }
        entry.state = HookState::Deactivated;
        entry.instance_id = None;
        tracing::info!(hook = %kind, instance = %instance_id, "deactivated hook");
        Ok(())
    }

    pub fn list(&self) -> Vec<HookEntry> {
        self.hooks.values().cloned().collect()
    }

    pub fn status(&self, kind: HookKind) -> Option<HookEntry> {
        self.hooks.get(&kind).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_str() {
        for k in [
            HookKind::MmTemplate,
            HookKind::UffdWp,
            HookKind::CgroupHigh,
            HookKind::EptLazyLoad,
        ] {
            let s = k.as_str();
            let parsed: HookKind = s.parse().expect("parse");
            assert_eq!(parsed, k);
        }
    }

    #[test]
    fn activate_then_deactivate() {
        let mut reg = HookRegistry::new();
        let inst = Uuid::new_v4();
        reg.register(HookKind::MmTemplate).expect("register");
        reg.activate(HookKind::MmTemplate, inst).expect("activate");
        let s = reg.status(HookKind::MmTemplate).expect("exists");
        assert_eq!(s.state, HookState::Activated);
        assert_eq!(s.instance_id, Some(inst));

        reg.deactivate(HookKind::MmTemplate, inst).expect("ok");
        assert_eq!(
            reg.status(HookKind::MmTemplate).expect("exists").state,
            HookState::Deactivated
        );
    }

    #[test]
    fn second_activation_is_blocked() {
        let mut reg = HookRegistry::new();
        reg.register(HookKind::UffdWp).expect("register");
        reg.activate(HookKind::UffdWp, Uuid::new_v4())
            .expect("first");
        let err = reg
            .activate(HookKind::UffdWp, Uuid::new_v4())
            .expect_err("blocked");
        assert!(matches!(err, BlazeError::HookError { .. }));
    }

    #[test]
    fn deactivate_by_wrong_owner_fails() {
        let mut reg = HookRegistry::new();
        let owner = Uuid::new_v4();
        reg.register(HookKind::CgroupHigh).expect("register");
        reg.activate(HookKind::CgroupHigh, owner).expect("ok");
        let err = reg
            .deactivate(HookKind::CgroupHigh, Uuid::new_v4())
            .expect_err("must not steal");
        assert!(matches!(err, BlazeError::HookError { .. }));
    }

    #[test]
    fn unregister_blocked_while_active() {
        let mut reg = HookRegistry::new();
        reg.register(HookKind::EptLazyLoad).expect("register");
        reg.activate(HookKind::EptLazyLoad, Uuid::new_v4())
            .expect("ok");
        let err = reg.unregister(HookKind::EptLazyLoad).expect_err("blocked");
        assert!(matches!(err, BlazeError::HookError { .. }));
    }
}
