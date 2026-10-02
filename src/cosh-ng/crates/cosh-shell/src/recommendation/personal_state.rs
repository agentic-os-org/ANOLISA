use std::path::PathBuf;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Instant;

use crate::raw_input::PromptGhostRoute;
use crate::runtime::state::{InlineState, PendingInputGhostBinding};

use super::personal_analysis_runtime::{AnalyzerCancellation, AnalyzerWorkerResult};
use super::personal_integration::PersonalSignalState;
use super::personal_runtime::PersonalRuntimeWriter;

#[derive(Default)]
pub(crate) struct PersonalizationState {
    pub(crate) writer: Option<PersonalRuntimeWriter>,
    pub(crate) writer_pending: Option<mpsc::Receiver<PersonalRuntimeWriter>>,
    pub(crate) store_root: Option<PathBuf>,
    pub(crate) configured_enabled: bool,
    pub(crate) environment_override: Option<bool>,
    pub(crate) history_file_pending: Option<mpsc::Receiver<PathBuf>>,
    pub(crate) history_file: Option<PathBuf>,
    pub(crate) history_synced_path: Option<PathBuf>,
    pub(crate) history_sync_pending: Option<(PathBuf, mpsc::Receiver<Result<(), String>>)>,
    pub(crate) history_retry_after: Option<(PathBuf, Instant)>,
    pub(crate) analyzer_cancellation: Option<AnalyzerCancellation>,
    pub(crate) analyzer_started: bool,
    pub(crate) analyzer_worker: Option<JoinHandle<AnalyzerWorkerResult>>,
    pub(crate) analyzer_last_attempt_generation: Option<u64>,
    pub(crate) foreground_model: Option<String>,
    pub(crate) idle_since: Option<Instant>,
    pub(crate) shell_input_active: bool,
    pub(crate) notice_shown: bool,
    pub(crate) startup_suppressed: bool,
    pub(crate) ai_disabled: bool,
    pub(crate) signals: PersonalSignalState,
    pub(crate) bash_history: bool,
}

impl PersonalizationState {
    pub(crate) fn request_analyzer_retry(&mut self) {
        self.analyzer_started = false;
        self.analyzer_last_attempt_generation = None;
    }

    pub(crate) fn poll_ready(&mut self) {
        if self.writer.is_some() {
            self.writer_pending = None;
            return;
        }
        let Some(receiver) = self.writer_pending.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(writer) => {
                self.writer = Some(writer);
                self.writer_pending = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.writer_pending = None;
            }
        }
    }

    pub(crate) fn poll_history_file(&mut self) {
        let Some(receiver) = self.history_file_pending.as_ref() else {
            return;
        };
        loop {
            match receiver.try_recv() {
                Ok(path) => self.history_file = Some(path),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.history_file_pending = None;
                    break;
                }
            }
        }
    }
}

impl InlineState {
    pub(crate) fn clear_personal_prompt_ghost(&mut self) -> bool {
        let had_personal_candidates = self
            .pending_prompt_suggestion_bindings
            .values()
            .any(|binding| matches!(binding, PendingInputGhostBinding::Personal(_)));
        let active_suggestion_id = match &self.pending_input_ghost_route {
            PromptGhostRoute::AgentSelection {
                candidates, active, ..
            } => candidates
                .get(*active)
                .map(|candidate| candidate.suggestion_id.clone()),
            _ => None,
        };
        self.pending_prompt_suggestion_bindings
            .retain(|_, binding| !matches!(binding, PendingInputGhostBinding::Personal(_)));
        let selection_route = matches!(
            self.pending_input_ghost_route,
            PromptGhostRoute::AgentSelection { .. }
        );
        let remaining = match self.pending_input_ghost_route.clone() {
            PromptGhostRoute::AgentSelection { candidates, .. } => candidates
                .into_iter()
                .filter(|candidate| {
                    self.pending_prompt_suggestion_bindings
                        .contains_key(&candidate.suggestion_id)
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        if had_personal_candidates && selection_route && !remaining.is_empty() {
            let active = active_suggestion_id
                .as_deref()
                .and_then(|id| {
                    remaining
                        .iter()
                        .position(|candidate| candidate.suggestion_id == id)
                })
                .unwrap_or(0);
            let selected = &remaining[active];
            self.pending_input_ghost = Some(selected.text.clone());
            self.pending_input_ghost_binding = self
                .pending_prompt_suggestion_bindings
                .get(&selected.suggestion_id)
                .cloned();
            self.pending_input_ghost_route = PromptGhostRoute::AgentSelection {
                candidates: remaining,
                active,
            };
        } else if had_personal_candidates
            && (selection_route
                || matches!(
                    self.pending_input_ghost_binding,
                    Some(PendingInputGhostBinding::Personal(_))
                ))
        {
            self.pending_input_ghost = None;
            self.pending_input_ghost_route = PromptGhostRoute::default();
            self.pending_input_ghost_binding = None;
        }
        if let Some(writer) = self.personalization.writer.as_mut() {
            writer.clear_frozen_prompt();
        }
        had_personal_candidates
    }
}
