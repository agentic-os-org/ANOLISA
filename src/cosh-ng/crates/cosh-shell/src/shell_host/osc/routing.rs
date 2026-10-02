use super::super::routing::routing_event;
use super::{now_ms, DisplayCutKind, Marker, OscParser};
use crate::types::{ShellEvent, ShellEventKind, ShellRoutingMetadata};

impl OscParser {
    pub(super) fn handle_routing_marker(
        &mut self,
        marker: Marker,
        session_id: String,
        timestamp: u64,
    ) {
        if marker.event == "intercept" {
            let generation = marker.generation;
            let top_level_missing = marker.top_level_missing.unwrap_or(false);
            let correlated = top_level_missing
                && generation.is_some()
                && self
                    .current
                    .as_ref()
                    .is_some_and(|current| current.attempt_generation == generation);
            if top_level_missing && !correlated {
                // Never let a stale command-owned marker degrade into an unbound direct intercept.
                return;
            }
            let input = marker.command.unwrap_or_default();
            let reason = marker
                .reason
                .unwrap_or_else(|| "natural_language".to_string());
            let sensitive = marker.sensitive.unwrap_or(false);
            self.intervention_cuts.push(self.clean.position());
            self.intervention_display_cuts
                .push((self.display.position(), DisplayCutKind::Intercept));
            // Record the display position at intercept time so that
            // `last_prompt_display()` returns only post-intercept bytes
            // (the new PS1 prompt), not the user-echoed command text that
            // bash already wrote to the terminal before the DEBUG trap fired.
            // Without this, RestorePrompt would re-emit the echoed command
            // below the panel, duplicating it on screen (#1811).
            self.start_prompt_display_capture();
            self.push_intercept_event_with_routing(
                &session_id,
                input,
                marker.cwd,
                &reason,
                generation,
                correlated,
                sensitive,
            );
            if correlated {
                self.current = None;
            }
            self.prompt_ready_display_start = None;
            return;
        }

        let generation = marker.generation;
        let correlated = marker.proven.unwrap_or(false)
            && generation.is_some()
            && self
                .current
                .as_ref()
                .is_some_and(|current| current.attempt_generation == generation);
        let command_id = correlated
            .then(|| self.current.as_ref().map(|command| command.id.clone()))
            .flatten();
        self.events.push(routing_event(
            session_id,
            command_id,
            marker.cwd,
            marker.intent,
            timestamp,
            ShellRoutingMetadata {
                generation: generation.unwrap_or_default(),
                top_level_missing: true,
                proven: correlated,
                sensitive: marker.sensitive.unwrap_or(false),
                unsafe_input: marker.unsafe_input.unwrap_or(false),
            },
        ));
    }

    pub(in crate::shell_host) fn push_intercept_event_with_routing(
        &mut self,
        session_id: &str,
        input: String,
        cwd: Option<String>,
        reason: &str,
        generation: Option<u64>,
        top_level_missing: bool,
        sensitive: bool,
    ) {
        let command_id = top_level_missing
            .then(|| self.current.as_ref().map(|command| command.id.clone()))
            .flatten();
        // Sensitive intercepts always carry routing metadata (even without
        // top-level-missing provenance) so the journal can redact the whole
        // input field before it reaches disk.
        let routing = (top_level_missing || sensitive).then_some(ShellRoutingMetadata {
            generation: generation.unwrap_or_default(),
            top_level_missing,
            proven: top_level_missing,
            sensitive,
            unsafe_input: false,
        });
        self.events.push(ShellEvent {
            kind: ShellEventKind::UserInputIntercepted,
            session_id: session_id.to_string(),
            command_id,
            command: None,
            cwd,
            end_cwd: None,
            exit_code: None,
            started_at_ms: Some(now_ms()),
            ended_at_ms: None,
            duration_ms: None,
            terminal_output_ref: None,
            terminal_output_bytes: None,
            input: Some(input),
            component: Some(reason.to_string()),
            message: Some("input intercepted".to_string()),
            command_origin: None,
            shell_environment_generation: None,
            audit_identity: None,
            routing,
            capture: None,
        });
    }
}
