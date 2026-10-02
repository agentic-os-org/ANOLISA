use std::fs;

use crate::input::AssistanceControl;
use crate::runtime::mode::render_mode_command;
use crate::runtime::state::InlineState;

fn routing_state(name: &str) -> (InlineState, std::path::PathBuf) {
    let state_file = std::env::temp_dir().join(format!(
        "cosh-routing-mode-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    fs::write(&state_file, b"enabled\n").expect("initial assistance state");
    let state = InlineState {
        assistance_control: Some(AssistanceControl::enabled(state_file.clone())),
        ..InlineState::default()
    };
    (state, state_file)
}

#[test]
fn routing_mode_command_updates_the_shared_enhanced_control() {
    let (mut state, state_file) = routing_state("shared-control");
    let mut output = Vec::new();

    assert!(render_mode_command(
        Some("routing"),
        Some("shell-only"),
        None,
        &mut state,
        &mut output,
    )
    .expect("set shell-only"));
    assert!(!state
        .assistance_control
        .as_ref()
        .expect("assistance control")
        .is_enabled());
    assert!(!state_file.exists());
    assert!(String::from_utf8_lossy(&output).contains("shell-only"));

    output.clear();
    assert!(render_mode_command(
        Some("routing"),
        Some("assisted"),
        None,
        &mut state,
        &mut output,
    )
    .expect("set assisted"));
    assert!(state
        .assistance_control
        .as_ref()
        .expect("assistance control")
        .is_enabled());
    assert!(state_file.is_file());
    assert!(String::from_utf8_lossy(&output).contains("assisted"));

    fs::remove_file(state_file).ok();
}

#[test]
fn routing_mode_command_rejects_runtime_switching_in_native_sessions() {
    let mut state = InlineState::default();
    let mut output = Vec::new();

    assert!(render_mode_command(
        Some("routing"),
        Some("assisted"),
        None,
        &mut state,
        &mut output,
    )
    .expect("render Native routing notice"));

    let rendered = String::from_utf8_lossy(&output);
    assert!(rendered.contains("Native"));
    assert!(rendered.contains("startup"));
}
