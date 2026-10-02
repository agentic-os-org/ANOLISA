use super::*;

#[test]
fn raw_cli_slow_agent_shows_elapsed_heartbeat() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (b"?? very slow agent\n".to_vec(), Duration::ZERO),
            (b"exit\n".to_vec(), Duration::from_millis(1800)),
        ],
    );

    assert!(!output.contains("Still working..."));
    assert!(!output.contains("Phase: thinking"));
    assert!(!output.contains("simulating a slow fake Agent run"));
    assert!(output.contains("Slow fake response for: ?? very slow agent"));
}

#[test]
fn raw_cli_animation_mode_uses_transient_agent_status() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_LANG", "en-US"),
            ("COSH_SHELL_ANIMATION", "always"),
            ("TERM", "xterm-256color"),
        ],
        vec![
            (b"?? slow agent\n".to_vec(), Duration::ZERO),
            (b"exit\n".to_vec(), Duration::from_millis(2_500)),
        ],
    );

    assert!(output.contains("⠋ Thinking..."), "{output}");
    assert!(output.contains("\x1b[2K"), "{output}");
    assert!(
        output.contains("Slow fake response for: ?? slow agent"),
        "{output}"
    );
}

#[test]
fn raw_cli_animation_stops_heartbeat_after_visible_agent_text() {
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_LANG", "en-US"),
            ("COSH_SHELL_ANIMATION", "always"),
            ("TERM", "xterm-256color"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$", b"?? slow text then wait\n"),
            ("Slow fake response for: ?? slow text then wait", b"exit\n"),
        ],
    );

    assert!(output.contains("⠋ Thinking..."), "{output}");
    assert!(
        output.contains("Slow fake response for: ?? slow text then wait"),
        "{output}"
    );
    assert!(!output.contains("receiving Agent response"), "{output}");
}
