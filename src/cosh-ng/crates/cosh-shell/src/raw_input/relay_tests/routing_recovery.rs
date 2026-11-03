use super::*;

#[test]
fn disabled_native_routing_controls_stay_local() {
    for command in [
        "/mode routing",
        "/mode routing assisted",
        "/mode routing shell-only",
        "/mode routing invalid",
    ] {
        for pending in [0, 1] {
            let (path, mut master) = output_file("disabled-native-routing-control");
            let state_file = path.with_extension("enabled");
            let control = AssistanceControl::enabled(state_file.clone());
            control.set_enabled(false).expect("disable assistance");
            let classifier = InputClassifier::default()
                .with_assistance_control(control)
                .with_bash_slash_submission_guard(true);
            let (tx, rx) = mpsc::channel();
            let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
            let gate = MainPromptGate::default();
            gate.set_at_prompt(pending == 0);
            let mut line_buffer = CandidateLineBuffer::default();
            let mut native_line_state = NativeLineState::default();
            let mut exit_tracker = ExplicitExitTracker::default();
            let generation = UserPtyInputGeneration::default();
            let mut submits = LineSubmitCounter::default();
            let mut relay = InputRelayContext {
                master: &mut master,
                input_classifier: &classifier,
                input_events: &tx,
                input_mode: &input_mode,
                input_generation: &generation,
                line_submits: &mut submits,
                line_buffer: &mut line_buffer,
                native_line_state: &mut native_line_state,
                exit_tracker: &mut exit_tracker,
                main_prompt_gate: &gate,
                slash_route_enabled: true,
                zsh_path_prompt_buffering: None,
            };
            relay_passthrough_input_after_shell_submits(
                format!("{command}\n").as_bytes(),
                pending,
                &mut relay,
            )
            .expect("relay routing control");
            let events = rx.try_iter().collect::<Vec<_>>();
            let controls = events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        RawInputEvent::UserIntercept(..)
                            | RawInputEvent::UserInterceptAtPrompt { .. }
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(
                controls.len(),
                1,
                "{command}, pending={pending}: {events:?}"
            );
            if pending == 0 {
                assert_eq!(
                    controls[0],
                    &RawInputEvent::UserIntercept(command.into(), InterceptReason::Slash)
                );
            } else {
                assert_eq!(
                    controls[0],
                    &RawInputEvent::UserInterceptAtPrompt {
                        input: command.into(),
                        reason: InterceptReason::Slash,
                        pending_submits: pending,
                    }
                );
            }
            assert!(
                fs::read(&path).expect("PTY bytes").is_empty(),
                "disabled recovery must never execute in Bash: {command}"
            );
            assert_eq!(gate.is_at_prompt(), pending == 0);
            assert!(!state_file.exists());
            fs::remove_file(path).expect("remove output");
        }
    }
}
