//! Session-manager state owned by the slash-command subsystem.
//!
//! Runtime state retains only this narrow handle while its legacy aggregate is split.

use std::collections::HashSet;

use crate::adapter::SessionSummary;

use super::compact::CompactionRuntime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionLaunchRequest {
    Picker,
    Resume(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeSessionPanelPhase {
    Browse,
    ConfirmClear,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeSessionPanel {
    pub(crate) id: String,
    pub(crate) workspace_scope: String,
    pub(crate) sessions: Vec<SessionSummary>,
    pub(crate) next_cursor: Option<String>,
    pub(crate) selected_option: usize,
    pub(crate) selected_for_clear: HashSet<String>,
    pub(crate) clear_confirmation_ids: Vec<String>,
    pub(crate) protected_clear_ids: Vec<String>,
    pub(crate) phase: RuntimeSessionPanelPhase,
}

#[derive(Default)]
pub(crate) struct SessionControlState {
    pending_panel: Option<RuntimeSessionPanel>,
    active_panel_id: Option<String>,
    active_panel_height: usize,
    handled_actions: HashSet<String>,
    pending_launch: Option<SessionLaunchRequest>,
    panel_sequence: usize,
    // Background compaction runtime; independent of the recovery machine.
    compaction: CompactionRuntime,
}

impl SessionControlState {
    pub(crate) fn compaction(&self) -> &CompactionRuntime {
        &self.compaction
    }

    pub(crate) fn compaction_mut(&mut self) -> &mut CompactionRuntime {
        &mut self.compaction
    }

    pub(crate) fn set_pending_panel(&mut self, panel: RuntimeSessionPanel) {
        self.pending_panel = Some(panel);
    }

    pub(crate) fn pending_panel(&self) -> Option<&RuntimeSessionPanel> {
        self.pending_panel.as_ref()
    }

    pub(crate) fn pending_panel_mut(&mut self) -> Option<&mut RuntimeSessionPanel> {
        self.pending_panel.as_mut()
    }

    pub(crate) fn clear_pending_panel(&mut self) {
        self.pending_panel = None;
    }

    pub(crate) fn claim_action(&mut self, key: String) -> bool {
        self.handled_actions.insert(key)
    }

    /// Allocates a panel ID that is never reused across panel lifecycles,
    /// so stale card events cannot address a newer panel.
    pub(crate) fn new_panel_id(&mut self) -> String {
        self.panel_sequence = self.panel_sequence.wrapping_add(1);
        format!("session-{}", self.panel_sequence)
    }

    pub(crate) fn active_panel_id(&self) -> Option<&str> {
        self.active_panel_id.as_deref()
    }

    pub(crate) fn set_active_panel(&mut self, id: String, height: usize) {
        self.active_panel_id = Some(id);
        self.active_panel_height = height;
    }

    pub(crate) fn active_panel_height(&self) -> usize {
        self.active_panel_height
    }

    pub(crate) fn clear_active_panel(&mut self) {
        self.active_panel_id = None;
        self.active_panel_height = 0;
    }

    pub(crate) fn clear_active_panel_id(&mut self) {
        self.active_panel_id = None;
    }

    pub(crate) fn set_pending_launch(&mut self, request: SessionLaunchRequest) {
        self.pending_launch = Some(request);
    }

    pub(crate) fn take_pending_launch(&mut self) -> Option<SessionLaunchRequest> {
        self.pending_launch.take()
    }
}
