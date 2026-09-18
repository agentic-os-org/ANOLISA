use std::sync::{Arc, Mutex};

use super::card_capture::{events::releases_capture, CardInputState};
use super::event_sender::RawInputEventSink;
use super::mode::RawInputMode;
use super::{RawInputCapture, RawInputEvent};

pub(super) struct CaptureConsumeResult {
    pub(super) generation: Option<u64>,
    pub(super) remainder: Vec<u8>,
    pub(super) retry: bool,
}

pub(super) fn consume_captured_input(
    card_state: &mut CardInputState,
    capture: &RawInputCapture,
    generation: u64,
    bytes: &[u8],
    input_events: &dyn RawInputEventSink,
    input_mode: &Arc<Mutex<RawInputMode>>,
) -> CaptureConsumeResult {
    let Ok(mut mode) = input_mode.lock() else {
        return CaptureConsumeResult {
            generation: None,
            remainder: bytes.to_vec(),
            retry: true,
        };
    };
    if !matches!(
        &*mode,
        RawInputMode::Capture {
            capture: active,
            generation: active_generation,
            ..
        } if active == capture && *active_generation == generation
    ) {
        // A stale snapshot must not wipe live selection state: when the
        // live mode still captures a card (e.g. the same approval card
        // re-armed under a new generation after an action-set switch),
        // re-align the card state to the live capture so apply_capture's
        // remap keeps the highlighted action; a different card resets
        // inside apply_capture, and only a non-capture mode drops the
        // state entirely.
        if let RawInputMode::Capture {
            capture: active, ..
        } = &*mode
        {
            card_state.apply_capture(active);
        } else {
            card_state.reset();
        }
        return CaptureConsumeResult {
            generation: None,
            remainder: bytes.to_vec(),
            retry: true,
        };
    }
    card_state.apply_capture(capture);
    let (events, remainder) = card_state.consume_split(capture, bytes);
    let released = events.iter().any(releases_capture);
    let submitted_generation = released.then_some(generation);
    if released {
        *mode = RawInputMode::Submitted {
            capture: capture.clone(),
            generation,
        };
        card_state.reset();
    }
    drop(mode);
    if let Some(generation) = submitted_generation {
        let (kind, target_id) = capture_target(capture);
        let _ = input_events.send(RawInputEvent::CaptureSubmitted {
            kind,
            target_id: target_id.to_string(),
            generation,
        });
    }
    for event in events {
        let _ = input_events.send(event);
    }
    CaptureConsumeResult {
        generation: submitted_generation,
        remainder,
        retry: false,
    }
}

fn capture_target(capture: &RawInputCapture) -> (&'static str, &str) {
    match capture {
        RawInputCapture::Question { id, .. } | RawInputCapture::TextQuestion { id, .. } => {
            ("question", id)
        }
        RawInputCapture::Approval { id, .. } => ("approval", id),
        RawInputCapture::Mode { id, .. } => ("mode", id),
        RawInputCapture::Config { id, .. } => ("config", id),
        RawInputCapture::ConfigLanguage { id, .. } => ("config_language", id),
        RawInputCapture::Session { id, .. } => ("session", id),
        RawInputCapture::Consultation { id } => ("consultation", id),
        RawInputCapture::Evidence { id } => ("evidence", id),
        RawInputCapture::PromptDraft { id, .. } => ("prompt_draft", id),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::raw_input::{update_input_mode, RawObserverAction};

    fn question() -> RawInputCapture {
        RawInputCapture::Question {
            id: "question-1".to_string(),
            option_count: 0,
            selected: 0,
            allow_free_text: true,
            multiple: false,
            secret: false,
        }
    }

    #[test]
    fn stale_capture_snapshot_retries_the_complete_chunk() {
        let capture = question();
        let input_mode = Arc::new(Mutex::new(RawInputMode::Capture {
            capture: capture.clone(),
            generation: 7,
            installed_at: std::time::Instant::now(),
        }));
        let (sender, receiver) = mpsc::channel();
        let mut state = CardInputState::default();

        let result =
            consume_captured_input(&mut state, &capture, 6, b"answer\n", &sender, &input_mode);

        assert!(result.retry);
        assert_eq!(result.remainder, b"answer\n");
        assert!(receiver.try_recv().is_err());
        assert!(matches!(
            &*input_mode.lock().expect("input mode"),
            RawInputMode::Capture { generation: 7, .. }
        ));
    }

    #[test]
    fn marked_session_enter_requests_delete_without_releasing_capture() {
        let capture = RawInputCapture::Session {
            id: "session-panel".to_string(),
            option_count: 2,
            selected: 0,
            marked_for_clear: vec![true, false],
            confirming_clear: false,
        };
        let input_mode = Arc::new(Mutex::new(RawInputMode::Capture {
            capture: capture.clone(),
            generation: 7,
            installed_at: std::time::Instant::now(),
        }));
        let (sender, receiver) = mpsc::channel();
        let mut state = CardInputState::default();

        let result = consume_captured_input(&mut state, &capture, 7, b"\n", &sender, &input_mode);

        assert_eq!(result.generation, None);
        assert!(result.remainder.is_empty());
        assert_eq!(
            receiver.recv().expect("session action"),
            RawInputEvent::SessionDelete("session-panel".to_string())
        );
        assert!(receiver.try_recv().is_err());
        assert!(matches!(
            &*input_mode.lock().expect("input mode"),
            RawInputMode::Capture { generation: 7, .. }
        ));
    }

    #[test]
    fn session_mark_refresh_preserves_generation_for_split_enter() {
        let unmarked = RawInputCapture::Session {
            id: "session-panel".to_string(),
            option_count: 2,
            selected: 0,
            marked_for_clear: vec![false, false],
            confirming_clear: false,
        };
        let marked = RawInputCapture::Session {
            id: "session-panel".to_string(),
            option_count: 2,
            selected: 0,
            marked_for_clear: vec![true, false],
            confirming_clear: false,
        };
        let input_mode = Arc::new(Mutex::new(RawInputMode::Capture {
            capture: unmarked.clone(),
            generation: 7,
            installed_at: std::time::Instant::now(),
        }));
        let (sender, receiver) = mpsc::channel();
        let mut state = CardInputState::default();

        let toggle = consume_captured_input(&mut state, &unmarked, 7, b" ", &sender, &input_mode);
        assert!(!toggle.retry);
        assert_eq!(toggle.generation, None);

        update_input_mode(
            &input_mode,
            &RawObserverAction::CaptureInput(marked.clone()),
            None,
        );
        assert!(matches!(
            &*input_mode.lock().expect("input mode"),
            RawInputMode::Capture {
                capture,
                generation: 7,
                ..
            } if capture == &marked
        ));

        let enter = consume_captured_input(&mut state, &marked, 7, b"\n", &sender, &input_mode);
        assert!(!enter.retry);
        assert_eq!(enter.generation, None);
        assert_eq!(
            receiver.try_iter().collect::<Vec<_>>(),
            vec![
                RawInputEvent::SessionToggle("session-panel".to_string(), 0),
                RawInputEvent::SessionDelete("session-panel".to_string()),
            ]
        );
    }

    /// Stale-generation 竞态回归（评审 P1 追加）：旧 generation 的 Standard
    /// snapshot 到达时，live 模式已把同一张卡切到 TurnConsent。mismatch
    /// 分支必须对齐到 live capture（重映射保留选择）而不是清空，否则重试
    /// 后回车会从 index 0 发出 CardApprove，与仍高亮 Deny 的卡面错位。
    #[test]
    fn stale_approval_snapshot_realigns_to_live_action_set() {
        let standard = RawInputCapture::Approval {
            id: "req-1".to_string(),
            action_set: crate::ui::ApprovalActionSet::Standard,
        };
        let turn = RawInputCapture::Approval {
            id: "req-1".to_string(),
            action_set: crate::ui::ApprovalActionSet::TurnConsent,
        };
        let input_mode = Arc::new(Mutex::new(RawInputMode::Capture {
            capture: turn.clone(),
            generation: 8,
            installed_at: std::time::Instant::now(),
        }));
        let (sender, receiver) = mpsc::channel();
        let mut state = CardInputState::default();
        // 在旧 generation 的 Standard 快照下选中 Deny（index 2）。
        state.apply_capture(&standard);
        state.consume(&standard, b"\x1b[C\x1b[C");

        // 旧快照到达：mismatch 分支对齐到 live TurnConsent，不清选择。
        let result = consume_captured_input(&mut state, &standard, 7, b"\n", &sender, &input_mode);
        assert!(result.retry);

        // 重试换用 live 快照：回车提交的仍是重映射后的 Deny。
        let result = consume_captured_input(&mut state, &turn, 8, b"\n", &sender, &input_mode);
        assert!(!result.retry);
        let events: Vec<_> = receiver.try_iter().collect();
        assert!(
            events.contains(&RawInputEvent::CardDeny("req-1".to_string())),
            "expected remapped Deny, got {events:?}"
        );
    }

    #[test]
    fn matching_capture_submits_under_the_same_lock() {
        let capture = question();
        let input_mode = Arc::new(Mutex::new(RawInputMode::Capture {
            capture: capture.clone(),
            generation: 7,
            installed_at: std::time::Instant::now(),
        }));
        let (sender, _receiver) = mpsc::channel();
        let mut state = CardInputState::default();

        let result =
            consume_captured_input(&mut state, &capture, 7, b"answer\n", &sender, &input_mode);

        assert!(!result.retry);
        assert_eq!(result.generation, Some(7));
        assert!(matches!(
            &*input_mode.lock().expect("input mode"),
            RawInputMode::Submitted { generation: 7, .. }
        ));
    }

    /// Esc-cancelling the prompt-draft card must release the relay-side
    /// capture like every other card (#1932): a capture left armed decays
    /// to Draining asynchronously and the late-input quarantine then
    /// swallows the first keystroke typed after the cancel (e.g. Up).
    #[test]
    fn prompt_draft_cancel_releases_the_capture_mode() {
        let capture = RawInputCapture::PromptDraft {
            id: "draft-1".to_string(),
            initial_text: "".into(),
            completion: None,
            agent_composer: false,
            workspace_cwd: None,
        };
        let input_mode = Arc::new(Mutex::new(RawInputMode::Capture {
            capture: capture.clone(),
            generation: 7,
            installed_at: std::time::Instant::now(),
        }));
        let (sender, receiver) = mpsc::channel();
        let mut state = CardInputState::default();

        let result =
            consume_captured_input(&mut state, &capture, 7, b"\x1b\x1b", &sender, &input_mode);

        assert!(!result.retry);
        assert_eq!(result.generation, Some(7));
        assert!(matches!(
            &*input_mode.lock().expect("input mode"),
            RawInputMode::Submitted { generation: 7, .. }
        ));
        let events: Vec<_> = receiver.try_iter().collect();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RawInputEvent::PromptDraftCancel { .. })),
            "{events:?}"
        );
    }

    /// Submitting the draft releases the capture the same way, so the
    /// Submitted -> Draining -> Terminal chain drains synchronously and
    /// input typed right after Enter is never quarantined (#1932).
    #[test]
    fn prompt_draft_submit_releases_the_capture_mode() {
        let capture = RawInputCapture::PromptDraft {
            id: "draft-1".to_string(),
            initial_text: "hello".into(),
            completion: None,
            agent_composer: false,
            workspace_cwd: None,
        };
        let input_mode = Arc::new(Mutex::new(RawInputMode::Capture {
            capture: capture.clone(),
            generation: 9,
            installed_at: std::time::Instant::now(),
        }));
        let (sender, _receiver) = mpsc::channel();
        let mut state = CardInputState::default();

        let result = consume_captured_input(&mut state, &capture, 9, b"\r", &sender, &input_mode);

        assert!(!result.retry);
        assert_eq!(result.generation, Some(9));
        assert!(matches!(
            &*input_mode.lock().expect("input mode"),
            RawInputMode::Submitted { generation: 9, .. }
        ));
    }
}
