use super::draft_editor::PromptDraftEditor;
use super::{RawInputCapture, RawInputEvent, CTRL_C};
use crate::question::choices::toggle_question_option;

use events::{
    approval_event_for_action, cancel_event, capture_action_set, capture_initial_selection,
    card_answer_event, empty_question_submission, is_csi_final_byte,
    is_removed_question_answer_slash, is_removed_question_answer_slash_fragment,
    question_choice_count, releases_capture, selected_options_answer,
};
// capture_bridge shares events::releases_capture with the consume loop (#1932).
pub(in crate::raw_input) mod events;
mod navigation;
mod prompt_draft;
mod text;

#[derive(Debug, Default)]
pub(super) struct CardInputState {
    selected: usize,
    free_text: String,
    active_kind: Option<CardInputKind>,
    selected_options: Vec<usize>,
    // Mirror toggles before emitting events so same-read Enter sees the
    // effective marks.
    session_marked_for_clear: Vec<bool>,
    pending_input: Vec<u8>,
    /// Multi-line draft state while a PromptDraft capture is active (#1721).
    draft: PromptDraftEditor,
    /// Bracketed paste passthrough inside the draft card: newlines paste as
    /// draft newlines instead of submitting.
    draft_paste: bool,
    draft_selection: Option<prompt_draft::DraftSlashSelection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CardInputKind {
    Question {
        id: String,
        option_count: usize,
        allow_free_text: bool,
        multiple: bool,
        secret: bool,
    },
    Approval {
        id: String,
        action_set: crate::ui::ApprovalActionSet,
    },
    Mode {
        id: String,
        option_count: usize,
    },
    Config {
        id: String,
        option_count: usize,
    },
    ConfigLanguage {
        id: String,
        option_count: usize,
    },
    Session {
        id: String,
        option_count: usize,
        confirming_clear: bool,
    },
    Evidence {
        id: String,
    },
    PromptDraft {
        id: String,
    },
}

impl CardInputState {
    pub(super) fn apply_capture(&mut self, capture: &RawInputCapture) {
        let kind = match capture {
            RawInputCapture::Question {
                id,
                option_count,
                allow_free_text,
                multiple,
                secret,
                ..
            } => CardInputKind::Question {
                id: id.clone(),
                option_count: *option_count,
                allow_free_text: *allow_free_text,
                multiple: *multiple,
                secret: *secret,
            },
            RawInputCapture::TextQuestion { id, secret, .. } => CardInputKind::Question {
                id: id.clone(),
                option_count: 0,
                allow_free_text: true,
                multiple: false,
                secret: *secret,
            },
            RawInputCapture::Approval { id, action_set } => CardInputKind::Approval {
                id: id.clone(),
                action_set: *action_set,
            },
            RawInputCapture::Consultation { id } => CardInputKind::Approval {
                id: id.clone(),
                action_set: crate::ui::ApprovalActionSet::Standard,
            },
            RawInputCapture::Mode {
                id, option_count, ..
            } => CardInputKind::Mode {
                id: id.clone(),
                option_count: *option_count,
            },
            RawInputCapture::Config {
                id, option_count, ..
            } => CardInputKind::Config {
                id: id.clone(),
                option_count: *option_count,
            },
            RawInputCapture::ConfigLanguage {
                id, option_count, ..
            } => CardInputKind::ConfigLanguage {
                id: id.clone(),
                option_count: *option_count,
            },
            RawInputCapture::Session {
                id,
                option_count,
                confirming_clear,
                ..
            } => CardInputKind::Session {
                id: id.clone(),
                option_count: *option_count,
                confirming_clear: *confirming_clear,
            },
            RawInputCapture::Evidence { id } => CardInputKind::Evidence { id: id.clone() },
            RawInputCapture::PromptDraft { id, .. } => {
                CardInputKind::PromptDraft { id: id.clone() }
            }
        };
        if self.active_kind.as_ref() != Some(&kind) {
            // Same approval card switching action sets (Standard <-> TurnConsent
            // as the pending queue grows or shrinks): remap the selection to the
            // previously highlighted action so Enter keeps submitting what the
            // card shows, instead of reinterpreting the stale index against the
            // new set. Actions missing from the new set fall back to Approve,
            // matching the render-side focus fallback.
            if let (
                Some(CardInputKind::Approval {
                    id: previous_id,
                    action_set: previous_set,
                }),
                CardInputKind::Approval { id, action_set },
            ) = (self.active_kind.as_ref(), &kind)
            {
                if previous_id == id {
                    let previous_action = previous_set.action_at(self.selected);
                    self.selected = previous_action
                        .and_then(|action| action_set.action_index(action))
                        .or_else(|| {
                            action_set.action_index(crate::ui::ApprovalPanelAction::Approve)
                        })
                        .unwrap_or(0);
                    self.active_kind = Some(kind);
                    return;
                }
            }
            let selected = capture_initial_selection(capture);
            self.active_kind = Some(kind);
            self.selected = selected;
            self.free_text = match capture {
                RawInputCapture::TextQuestion { initial_text, .. } => initial_text.clone(),
                _ => String::new(),
            };
            self.selected_options.clear();
            self.session_marked_for_clear = match capture {
                RawInputCapture::Session {
                    marked_for_clear, ..
                } => marked_for_clear.clone(),
                _ => Vec::new(),
            };
            self.pending_input.clear();
            self.draft = match capture {
                RawInputCapture::PromptDraft { initial_text, .. } => {
                    PromptDraftEditor::from_text(initial_text)
                }
                _ => PromptDraftEditor::default(),
            };
            self.draft_paste = false;
        }
    }

    pub(super) fn reset(&mut self) {
        self.active_kind = None;
        self.selected = 0;
        self.free_text.clear();
        self.selected_options.clear();
        self.session_marked_for_clear.clear();
        self.pending_input.clear();
        self.draft = PromptDraftEditor::default();
        self.draft_paste = false;
        self.draft_selection = None;
    }

    pub(super) fn consume(
        &mut self,
        capture: &RawInputCapture,
        bytes: &[u8],
    ) -> Vec<RawInputEvent> {
        self.consume_split(capture, bytes).0
    }

    pub(super) fn consume_split(
        &mut self,
        capture: &RawInputCapture,
        bytes: &[u8],
    ) -> (Vec<RawInputEvent>, Vec<u8>) {
        let mut events = Vec::new();
        let mut input = Vec::new();
        if self.pending_input.is_empty() {
            input.extend_from_slice(bytes);
        } else {
            input.append(&mut self.pending_input);
            input.extend_from_slice(bytes);
        }
        let mut idx = 0;
        while idx < input.len() {
            match input[idx] {
                CTRL_C => {
                    match capture {
                        RawInputCapture::Approval { id, .. }
                        | RawInputCapture::Consultation { id } => {
                            events.push(RawInputEvent::CardCancel(id.clone()))
                        }
                        RawInputCapture::Mode { id, .. } => {
                            events.push(RawInputEvent::ModeCancel(id.clone()))
                        }
                        RawInputCapture::Config { id, .. } => {
                            events.push(RawInputEvent::ConfigCancel(id.clone()))
                        }
                        RawInputCapture::ConfigLanguage { id, .. } => {
                            events.push(RawInputEvent::ConfigLanguageCancel(id.clone()))
                        }
                        RawInputCapture::Session { id, .. } => {
                            events.push(RawInputEvent::SessionCancel(id.clone()))
                        }
                        // Ctrl+C abandons the prompt outright. A capture is all that reaches the
                        // relay here, so this is the only place the interrupt can be told apart
                        // from the ESC arms below.
                        RawInputCapture::Question { id, .. }
                        | RawInputCapture::TextQuestion { id, .. } => {
                            events.push(RawInputEvent::QuestionAbort(id.clone()))
                        }
                        RawInputCapture::Evidence { id } => {
                            events.push(RawInputEvent::EvidenceCancel(id.clone()))
                        }
                        RawInputCapture::PromptDraft { id, .. } => {
                            events.push(RawInputEvent::PromptDraftCancel { id: id.clone() })
                        }
                    }
                    idx += 1;
                }
                b'\r' | b'\n' => {
                    if matches!(capture, RawInputCapture::PromptDraft { .. }) && self.draft_paste {
                        idx = self.draft_pasted_newline(capture, &input, idx, &mut events);
                        continue;
                    }
                    events.extend(self.draft_complete_command_on_submit(capture));
                    let event = self.submit(capture);
                    let submitted = event.is_some();
                    events.extend(event);
                    // Always clear free_text after a submit attempt so that a
                    // subsequent capture (e.g. the next auth field) starts with
                    // a clean buffer.  The outer consume_captured_input also
                    // calls reset() on release, but clearing here covers any
                    // non-release submit (e.g. QuestionSubmitAttempt) and
                    // eliminates reliance on preserve_for_retry which could
                    // leak text across captures when reset() is bypassed.
                    self.free_text.clear();
                    idx += 1;
                    if submitted {
                        // State transitions are applied by the main loop; suppress a burst of
                        // duplicate submits against the capture snapshot used for this batch.
                        break;
                    }
                }
                0x7f | 0x08 => {
                    if matches!(capture, RawInputCapture::PromptDraft { .. }) {
                        self.draft.backspace();
                        if let Some(event) = self.input_event(capture) {
                            events.push(event);
                        }
                    } else if self.free_text.pop().is_some() {
                        if let Some(event) = self.input_event(capture) {
                            events.push(event);
                        }
                    }
                    idx += 1;
                }
                0x01 | 0x05 | 0x15 if matches!(capture, RawInputCapture::PromptDraft { .. }) => {
                    self.draft_control_key(capture, input[idx], &mut events);
                    idx += 1;
                }
                0x09 => {
                    if !self.draft_pasted_tab(capture, &mut events) {
                        if let Some(event) = self.apply_tab(capture) {
                            events.push(event);
                        }
                    }
                    idx += 1;
                }
                0x1b if input.get(idx + 1) == Some(&b'[') => {
                    let Some(next_idx) =
                        self.consume_csi_sequence(capture, &input, idx, &mut events)
                    else {
                        self.pending_input.extend_from_slice(&input[idx..]);
                        break;
                    };
                    idx = next_idx;
                }
                0x1b if input.get(idx + 1) == Some(&b'O') => {
                    if input.get(idx + 2).is_none() {
                        self.pending_input.extend_from_slice(&input[idx..]);
                        break;
                    }
                    if let Some(event) = self.apply_arrow(capture, input[idx + 2]) {
                        events.push(event);
                    }
                    idx += 3;
                }
                0x1b if input.get(idx + 1) == Some(&0x1b) => {
                    match capture {
                        RawInputCapture::Approval { id, .. }
                        | RawInputCapture::Consultation { id } => {
                            events.push(RawInputEvent::CardCancel(id.clone()))
                        }
                        RawInputCapture::Mode { id, .. } => {
                            events.push(RawInputEvent::ModeCancel(id.clone()))
                        }
                        RawInputCapture::Config { id, .. } => {
                            events.push(RawInputEvent::ConfigCancel(id.clone()))
                        }
                        RawInputCapture::ConfigLanguage { id, .. } => {
                            events.push(RawInputEvent::ConfigLanguageCancel(id.clone()))
                        }
                        RawInputCapture::Session { id, .. } => {
                            events.push(RawInputEvent::SessionCancel(id.clone()))
                        }
                        RawInputCapture::Question { id, .. }
                        | RawInputCapture::TextQuestion { id, .. } => {
                            events.push(RawInputEvent::QuestionCancel(id.clone()))
                        }
                        RawInputCapture::Evidence { id } => {
                            events.push(RawInputEvent::EvidenceCancel(id.clone()))
                        }
                        RawInputCapture::PromptDraft { id, .. } => {
                            events.push(RawInputEvent::PromptDraftCancel { id: id.clone() })
                        }
                    }
                    idx += 2;
                }
                0x1b if input.get(idx + 1).is_none() => {
                    if self.draft_hold_bare_escape(capture) {
                        idx += 1;
                        break;
                    }
                    events.push(cancel_event(capture));
                    idx += 1;
                    break;
                }
                0x1b => match capture {
                    RawInputCapture::Approval { id, .. } | RawInputCapture::Consultation { id } => {
                        events.push(RawInputEvent::CardCancel(id.clone()));
                        idx += 1;
                        break;
                    }
                    RawInputCapture::Mode { id, .. } => {
                        events.push(RawInputEvent::ModeCancel(id.clone()));
                        idx += 1;
                        break;
                    }
                    RawInputCapture::Config { id, .. } => {
                        events.push(RawInputEvent::ConfigCancel(id.clone()));
                        idx += 1;
                        break;
                    }
                    RawInputCapture::ConfigLanguage { id, .. } => {
                        events.push(RawInputEvent::ConfigLanguageCancel(id.clone()));
                        idx += 1;
                        break;
                    }
                    RawInputCapture::Session { id, .. } => {
                        events.push(RawInputEvent::SessionCancel(id.clone()));
                        idx += 1;
                        break;
                    }
                    RawInputCapture::Question { id, .. }
                    | RawInputCapture::TextQuestion { id, .. } => {
                        events.push(RawInputEvent::QuestionCancel(id.clone()));
                        idx += 1;
                        break;
                    }
                    RawInputCapture::Evidence { id } => {
                        events.push(RawInputEvent::EvidenceCancel(id.clone()));
                        idx += 1;
                        break;
                    }
                    RawInputCapture::PromptDraft { id, .. } => {
                        let (next, cancelled) =
                            self.draft_escape_sequence(capture, id, &input, idx, &mut events);
                        idx = next;
                        if cancelled {
                            break;
                        }
                    }
                },
                byte if !byte.is_ascii_control() => match capture {
                    RawInputCapture::PromptDraft { .. } => {
                        idx = self.draft_insert_visible(capture, &input, idx, &mut events);
                        continue;
                    }
                    RawInputCapture::Evidence { id } => {
                        if byte == b'i' || byte == b'I' {
                            events.push(RawInputEvent::EvidenceIgnore(id.clone()));
                        }
                        idx += 1;
                    }
                    RawInputCapture::Approval { id, .. } | RawInputCapture::Consultation { id } => {
                        if (byte == b'd' || byte == b'D') && self.free_text.is_empty() {
                            self.selected = 2;
                            events.push(RawInputEvent::CardDetails(id.clone()));
                        } else if byte.is_ascii() {
                            self.free_text.push(byte as char);
                        } else {
                            let start = idx;
                            while idx < input.len()
                                && !input[idx].is_ascii_control()
                                && input[idx] != 0x1b
                            {
                                idx += 1;
                            }
                            self.free_text
                                .push_str(&String::from_utf8_lossy(&input[start..idx]));
                            continue;
                        }
                        idx += 1;
                    }
                    RawInputCapture::Mode { .. }
                    | RawInputCapture::Config { .. }
                    | RawInputCapture::ConfigLanguage { .. } => {
                        idx += 1;
                    }
                    RawInputCapture::Session {
                        id,
                        option_count,
                        confirming_clear,
                        ..
                    } => {
                        match byte {
                            b'j' | b'J' if !*confirming_clear => {
                                let previous = self.selected;
                                self.selected =
                                    (self.selected + 1).min(option_count.saturating_sub(1));
                                if self.selected != previous {
                                    events.push(RawInputEvent::SessionFocus(
                                        id.clone(),
                                        self.selected,
                                    ));
                                }
                            }
                            b'k' | b'K' if !*confirming_clear => {
                                let previous = self.selected;
                                self.selected = self.selected.saturating_sub(1);
                                if self.selected != previous {
                                    events.push(RawInputEvent::SessionFocus(
                                        id.clone(),
                                        self.selected,
                                    ));
                                }
                            }
                            b' ' if !*confirming_clear && self.selected < *option_count => {
                                if let Some(marked) =
                                    self.session_marked_for_clear.get_mut(self.selected)
                                {
                                    *marked = !*marked;
                                }
                                events
                                    .push(RawInputEvent::SessionToggle(id.clone(), self.selected));
                            }
                            b'd' | b'D' if !*confirming_clear => {
                                events.push(RawInputEvent::SessionDelete(id.clone()));
                            }
                            b'y' | b'Y' if *confirming_clear => {
                                events.push(RawInputEvent::SessionClearConfirm(id.clone()));
                            }
                            b'n' | b'N' if *confirming_clear => {
                                events.push(RawInputEvent::SessionCancel(id.clone()));
                            }
                            _ => {}
                        }
                        idx += 1;
                    }
                    RawInputCapture::Question {
                        id,
                        option_count,
                        multiple,
                        ..
                    } => {
                        if *multiple
                            && byte == b' '
                            && self.selected < *option_count
                            && self.free_text.is_empty()
                        {
                            toggle_question_option(&mut self.selected_options, self.selected);
                            events.push(RawInputEvent::CardToggle(id.clone(), self.selected));
                            idx += 1;
                            continue;
                        }
                        if self.append_free_text(&input, &mut idx) {
                            events.extend(self.input_event(capture));
                        }
                    }
                    RawInputCapture::TextQuestion { .. } => {
                        if self.append_free_text(&input, &mut idx) {
                            events.extend(self.input_event(capture));
                        }
                    }
                },
                _ => {
                    idx += 1;
                }
            }
            if events.last().is_some_and(releases_capture) {
                break;
            }
        }
        (events, input[idx..].to_vec())
    }

    fn submit(&self, capture: &RawInputCapture) -> Option<RawInputEvent> {
        match capture {
            RawInputCapture::Question {
                id,
                option_count,
                allow_free_text,
                multiple,
                secret,
                ..
            } => {
                let answer = self.free_text.trim();
                if is_removed_question_answer_slash(answer) {
                    return None;
                }
                if *multiple {
                    if !answer.is_empty() && *allow_free_text {
                        if self.selected_options.is_empty() {
                            return Some(card_answer_event(&format!("\n{answer}"), *secret));
                        }
                        return Some(card_answer_event(
                            &format!(
                                "{}\n{}",
                                selected_options_answer(&self.selected_options),
                                answer
                            ),
                            *secret,
                        ));
                    }
                    if self.selected_options.is_empty() {
                        return Some(empty_question_submission(id, *secret));
                    }
                    return Some(card_answer_event(
                        &selected_options_answer(&self.selected_options),
                        *secret,
                    ));
                }
                if !answer.is_empty() && *allow_free_text {
                    return Some(card_answer_event(answer, *secret));
                }
                if self.selected < *option_count {
                    return Some(card_answer_event(&(self.selected + 1).to_string(), *secret));
                }
                if !answer.is_empty() {
                    return Some(card_answer_event(answer, *secret));
                }
                if *allow_free_text && self.selected == *option_count {
                    return Some(empty_question_submission(id, *secret));
                }
                if *allow_free_text && *option_count == 0 {
                    return Some(empty_question_submission(id, *secret));
                }
                None
            }
            RawInputCapture::TextQuestion { id, secret, .. } => {
                let answer = self.free_text.trim();
                if is_removed_question_answer_slash(answer) {
                    return None;
                }
                if answer.is_empty() {
                    Some(empty_question_submission(id, *secret))
                } else {
                    Some(card_answer_event(answer, *secret))
                }
            }
            RawInputCapture::Approval { id, .. } | RawInputCapture::Consultation { id } => {
                if !self.free_text.trim().is_empty() {
                    return None;
                }
                let action = capture_action_set(capture).action_at(self.selected);
                action.map(|a| approval_event_for_action(id, a))
            }
            RawInputCapture::Mode {
                id, option_count, ..
            } => {
                if *option_count == 0 || self.selected >= *option_count {
                    return None;
                }
                Some(RawInputEvent::ModeSet(id.clone(), self.selected))
            }
            RawInputCapture::Config {
                id, option_count, ..
            } => {
                if *option_count == 0 || self.selected >= *option_count {
                    return None;
                }
                if self.selected == 0 {
                    Some(RawInputEvent::ConfigSave(id.clone()))
                } else {
                    Some(RawInputEvent::ConfigCancel(id.clone()))
                }
            }
            RawInputCapture::ConfigLanguage {
                id, option_count, ..
            } => {
                if *option_count == 0 || self.selected >= *option_count {
                    return None;
                }
                Some(RawInputEvent::ConfigLanguageSet(id.clone(), self.selected))
            }
            RawInputCapture::Session {
                id,
                option_count,
                confirming_clear,
                ..
            } => {
                if *confirming_clear {
                    return Some(RawInputEvent::SessionClearConfirm(id.clone()));
                }
                if self.session_marked_for_clear.iter().any(|marked| *marked) {
                    return Some(RawInputEvent::SessionDelete(id.clone()));
                }
                if *option_count == 0 || self.selected >= *option_count {
                    return None;
                }
                Some(RawInputEvent::SessionResume(id.clone(), self.selected))
            }
            RawInputCapture::Evidence { id } => Some(RawInputEvent::EvidenceSend(id.clone())),
            RawInputCapture::PromptDraft { id, .. } => self.draft_submit_event(id, capture),
        }
    }

    fn input_event(&self, capture: &RawInputCapture) -> Option<RawInputEvent> {
        match capture {
            RawInputCapture::Question {
                id,
                allow_free_text,
                secret,
                ..
            } if *allow_free_text => {
                if is_removed_question_answer_slash_fragment(&self.free_text) {
                    return None;
                }
                if *secret {
                    Some(RawInputEvent::CardSecretInput(
                        id.clone(),
                        self.free_text.clone(),
                    ))
                } else {
                    Some(RawInputEvent::CardInput(id.clone(), self.free_text.clone()))
                }
            }
            RawInputCapture::TextQuestion { id, secret, .. } => {
                if is_removed_question_answer_slash_fragment(&self.free_text) {
                    return None;
                }
                if *secret {
                    Some(RawInputEvent::CardSecretInput(
                        id.clone(),
                        self.free_text.clone(),
                    ))
                } else {
                    Some(RawInputEvent::CardInput(id.clone(), self.free_text.clone()))
                }
            }
            RawInputCapture::PromptDraft { id, .. } => Some(self.draft_changed_event(id)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
