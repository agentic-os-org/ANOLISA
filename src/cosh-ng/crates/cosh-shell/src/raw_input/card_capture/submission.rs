use super::*;

impl CardInputState {
    pub(in crate::raw_input) fn apply_capture_generation(
        &mut self,
        capture: &RawInputCapture,
        generation: u64,
    ) {
        if self.active_generation != Some(generation)
            && matches!(capture, RawInputCapture::Approval { .. })
        {
            self.finish_submission(capture);
        }
        self.apply_capture(capture);
        self.active_generation = Some(generation);
    }

    pub(in crate::raw_input) fn reset(&mut self) {
        self.active_kind = None;
        self.active_generation = None;
        self.selected = 0;
        self.free_text.clear();
        self.selected_options.clear();
        self.session_marked_for_clear.clear();
        self.pending_input.clear();
        self.draft = PromptDraftEditor::default();
        self.draft_paste = false;
        self.draft_selection = None;
    }

    pub(in crate::raw_input) fn finish_submission(&mut self, capture: &RawInputCapture) {
        let active_kind = self.active_kind.take();
        let selected = self.selected;
        self.reset();
        if matches!(capture, RawInputCapture::Approval { .. }) {
            // A replacement of the same pending approval must keep its selection.
            self.active_kind = active_kind;
            self.selected = selected;
        }
    }
}
