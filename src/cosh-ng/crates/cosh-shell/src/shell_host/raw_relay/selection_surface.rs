use crate::raw_input::SuggestionSurfacePayload;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::shell_host) enum SurfaceState {
    Idle,
    Live,
    PromptLocal,
    Deferred,
    Draft,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::shell_host) enum CyclePlan {
    RedrawList { lines: Vec<String> },
    PromptLocal { label: String },
    Ignore,
}

pub(in crate::shell_host) struct SelectionSurface {
    state: SurfaceState,
    payload: Option<SuggestionSurfacePayload>,
    active: usize,
    generation: u64,
    anchor_display: usize,
    anchor_prompt: String,
    rows_total: usize,
    cur_width: u16,
    cur_height: u16,
}

impl Default for SelectionSurface {
    fn default() -> Self {
        Self {
            state: SurfaceState::Idle,
            payload: None,
            active: 0,
            generation: 0,
            anchor_display: 0,
            anchor_prompt: String::new(),
            rows_total: 0,
            cur_width: 0,
            cur_height: 0,
        }
    }
}

impl SelectionSurface {
    pub(in crate::shell_host) fn state(&self) -> SurfaceState {
        self.state
    }

    pub(in crate::shell_host) fn publish(
        &mut self,
        payload: SuggestionSurfacePayload,
        active: usize,
        generation: u64,
        prompt: &str,
        display_position: usize,
    ) {
        self.rows_total = payload.rows + prompt_rows(prompt, payload.width);
        self.cur_width = payload.width;
        self.cur_height = payload.height;
        let fits = payload.rows > 0 && self.rows_total <= payload.height as usize;
        self.state = if payload.labels.is_empty() {
            SurfaceState::Idle
        } else if fits {
            SurfaceState::Live
        } else {
            SurfaceState::PromptLocal
        };
        self.payload = Some(payload);
        self.active = active;
        self.generation = generation;
        self.anchor_display = display_position;
        self.anchor_prompt = prompt.to_string();
    }

    pub(in crate::shell_host) fn retire(&mut self) {
        self.state = SurfaceState::Idle;
        self.payload = None;
        self.active = 0;
    }

    pub(in crate::shell_host) fn invalidate(&mut self) {
        if self.state == SurfaceState::Live {
            self.state = SurfaceState::PromptLocal;
        }
    }

    pub(in crate::shell_host) fn defer(&mut self) {
        if self.state != SurfaceState::Idle {
            self.state = SurfaceState::Deferred;
        }
    }

    pub(in crate::shell_host) fn note_draft(&mut self) {
        if self.state != SurfaceState::Idle {
            self.state = SurfaceState::Draft;
        }
    }

    pub(in crate::shell_host) fn note_resize(&mut self, width: u16, height: u16) {
        let width_changed = width != self.cur_width;
        self.cur_width = width;
        self.cur_height = height;
        if self.state == SurfaceState::Idle || self.state == SurfaceState::Deferred {
            return;
        }
        if self.payload.is_some() {
            self.state = if width_changed {
                SurfaceState::PromptLocal
            } else if self.rows_total <= height as usize {
                SurfaceState::Live
            } else {
                SurfaceState::PromptLocal
            };
        }
    }

    pub(in crate::shell_host) fn plan_cycle(
        &mut self,
        suggestion_id: Option<&str>,
        generation: u64,
        prompt: &str,
        display_position: usize,
        width: u16,
        height: u16,
    ) -> CyclePlan {
        let Some(payload) = &self.payload else {
            return CyclePlan::Ignore;
        };
        if self.state == SurfaceState::Idle
            || self.state == SurfaceState::Deferred
            || self.state == SurfaceState::Draft
            || payload.labels.is_empty()
            || generation != self.generation
        {
            return CyclePlan::Ignore;
        }
        let next = (self.active + 1) % payload.labels.len();
        if suggestion_id != Some(payload.ids[next].as_str()) {
            return CyclePlan::Ignore;
        }
        self.active = next;
        if prompt != self.anchor_prompt
            || display_position != self.anchor_display
            || width != self.cur_width
            || height != self.cur_height
        {
            self.state = SurfaceState::PromptLocal;
        }
        if self.state == SurfaceState::Live && self.rows_total > height as usize {
            self.state = SurfaceState::PromptLocal;
        }
        match self.state {
            SurfaceState::Live => CyclePlan::RedrawList {
                lines: payload.variants[next].clone(),
            },
            SurfaceState::PromptLocal => CyclePlan::PromptLocal {
                label: format!("{}.", next + 1),
            },
            _ => CyclePlan::Ignore,
        }
    }
}

fn prompt_rows(prompt: &str, width: u16) -> usize {
    use unicode_width::UnicodeWidthStr;
    let columns = width.max(1) as usize;
    prompt.width().div_ceil(columns).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(count: usize, rows: usize, height: u16) -> SuggestionSurfacePayload {
        SuggestionSurfacePayload {
            variants: (0..count)
                .map(|active| vec![format!("line {active}")])
                .collect(),
            labels: (0..count)
                .map(|index| format!("{}. q{index}", index + 1))
                .collect(),
            ids: (0..count)
                .map(|index| format!("candidate-{index}"))
                .collect(),
            rows,
            width: 80,
            height,
        }
    }

    fn published(count: usize, rows: usize, height: u16) -> SelectionSurface {
        let mut surface = SelectionSurface::default();
        surface.publish(payload(count, rows, height), 0, 7, "prompt$ ", 100);
        surface
    }

    #[test]
    fn live_surface_redraws_only_for_the_next_candidate_of_its_generation() {
        let mut surface = published(3, 6, 40);
        assert_eq!(surface.state(), SurfaceState::Live);
        assert!(matches!(
            surface.plan_cycle(Some("candidate-1"), 7, "prompt$ ", 100, 80, 40),
            CyclePlan::RedrawList { .. }
        ));
        assert!(matches!(
            surface.plan_cycle(Some("candidate-1"), 7, "prompt$ ", 100, 80, 40),
            CyclePlan::Ignore
        ));
        assert!(matches!(
            surface.plan_cycle(Some("candidate-2"), 8, "prompt$ ", 100, 80, 40),
            CyclePlan::Ignore
        ));
        assert!(matches!(
            surface.plan_cycle(Some("candidate-2"), 7, "prompt$ ", 100, 80, 40),
            CyclePlan::RedrawList { .. }
        ));
    }

    #[test]
    fn lost_anchor_or_foreign_output_falls_back_to_prompt_local_without_erasing_rows() {
        let mut surface = published(2, 6, 40);
        surface.invalidate();
        match surface.plan_cycle(Some("candidate-1"), 7, "prompt$ ", 100, 80, 40) {
            CyclePlan::PromptLocal { label } => assert_eq!(label, "2."),
            other => panic!("expected prompt-local plan, got {other:?}"),
        }
        let mut resized = published(2, 6, 40);
        assert!(matches!(
            resized.plan_cycle(Some("candidate-1"), 7, "prompt$ ", 100, 80, 20),
            CyclePlan::PromptLocal { .. }
        ));
    }

    #[test]
    fn oversized_lists_publish_prompt_local_and_keep_every_candidate_selectable() {
        let mut surface = published(1, 40, 24);
        assert_eq!(surface.state(), SurfaceState::PromptLocal);
        assert!(matches!(
            surface.plan_cycle(Some("candidate-0"), 7, "prompt$ ", 100, 80, 24),
            CyclePlan::PromptLocal { .. }
        ));
    }

    #[test]
    fn resize_reproves_geometry_and_restores_live_without_new_publish() {
        let mut surface = published(2, 6, 6);
        assert_eq!(surface.state(), SurfaceState::PromptLocal);
        surface.note_resize(80, 40);
        assert_eq!(surface.state(), SurfaceState::Live);
        assert!(matches!(
            surface.plan_cycle(Some("candidate-1"), 7, "prompt$ ", 100, 80, 40),
            CyclePlan::RedrawList { .. }
        ));
        surface.note_resize(80, 6);
        assert_eq!(surface.state(), SurfaceState::PromptLocal);
    }

    #[test]
    fn width_change_demotes_live_region_until_republish() {
        let mut surface = published(2, 6, 40);
        assert_eq!(surface.state(), SurfaceState::Live);
        surface.note_resize(40, 40);
        assert_eq!(surface.state(), SurfaceState::PromptLocal);
        assert!(matches!(
            surface.plan_cycle(Some("candidate-1"), 7, "prompt$ ", 100, 40, 40),
            CyclePlan::PromptLocal { .. }
        ));
    }

    #[test]
    fn draft_ownership_blocks_cycle_until_republish() {
        let mut surface = published(2, 6, 40);
        surface.note_draft();
        assert_eq!(surface.state(), SurfaceState::Draft);
        assert!(matches!(
            surface.plan_cycle(Some("candidate-1"), 7, "prompt$ ", 100, 80, 40),
            CyclePlan::Ignore
        ));
    }

    #[test]
    fn deferred_and_retired_surfaces_never_draw() {
        let mut surface = published(2, 6, 40);
        surface.defer();
        assert!(matches!(
            surface.plan_cycle(Some("candidate-1"), 7, "prompt$ ", 100, 80, 40),
            CyclePlan::Ignore
        ));
        surface.retire();
        assert_eq!(surface.state(), SurfaceState::Idle);
        assert!(matches!(
            surface.plan_cycle(Some("candidate-0"), 7, "prompt$ ", 100, 80, 40),
            CyclePlan::Ignore
        ));
    }
}
