use super::*;

#[test]
fn raw_cli_agent_question_accepts_card_answer_choice() {
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$ ", b"?? ask question\n"),
            ("Left/Right move", b"\x1b[C\n"),
            ("Got your answer: Blue", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains("Agent question"), "{output}");
    assert!(!output.contains("Agent question q-1"), "{output}");
    assert!(
        output.contains("Choose a color for the next step"),
        "{output}"
    );
    assert!(output.contains("Select one:"), "{output}");
    assert!(output.contains("[1] Green"), "{output}");
    assert!(output.contains("[2] Blue"), "{output}");
    assert!(output.contains("[4] Other..."), "{output}");
    assert!(output.contains("> [1] Green"), "{output}");
    assert!(output.contains("> [2] Blue"), "{output}");
    assert!(output.contains("Left/Right move"), "{output}");
    assert!(output.contains("Enter send"), "{output}");
    assert!(!output.contains("│Choose"), "{output}");
    assert!(!output.contains("│Options"), "{output}");
    assert!(!output.contains("Agent is asking for input"), "{output}");
    assert!(!output.contains("Effect: answer is sent back"), "{output}");
    assert!(output.contains("Answer: Blue"), "{output}");
    assert!(!output.contains("Answer sent"), "{output}");
    assert!(output.contains("Answer: Blue"), "{output}");
    assert!(!output.contains("Question: Choose a color"), "{output}");
    assert!(!output.contains("│Question"), "{output}");
    assert!(!output.contains("│Answer"), "{output}");
    assert!(!output.contains("Sent to Agent"), "{output}");
    assert!(!output.contains("no command ran"), "{output}");
    assert!(
        !output.contains("Continuing the same Agent session"),
        "{output}"
    );
    assert!(output.contains("Got your answer: Blue"), "{output}");
    assert!(!output.contains("/answer"), "{output}");
    assert!(!output.contains("bash:"), "{output}");
}

#[test]
fn raw_cli_agent_question_uses_zh_language_env() {
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "zh-CN")],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$ ", b"?? ask question\n"),
            ("Enter \u{53d1}\u{9001}", b"\x1b[C\n"),
            ("Got your answer: Blue", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains("Agent 问题"), "{output}");
    assert!(!output.contains("Agent 问题 q-1"), "{output}");
    assert!(
        output.contains("Choose a color for the next step"),
        "{output}"
    );
    assert!(output.contains("选择一项:"), "{output}");
    assert!(output.contains("[1] Green"), "{output}");
    assert!(output.contains("[2] Blue"), "{output}");
    assert!(output.contains("[4] 其他..."), "{output}");
    assert!(output.contains("> [2] Blue"), "{output}");
    assert!(output.contains("左/右移动"), "{output}");
    assert!(output.contains("Enter 发送"), "{output}");
    assert!(output.contains("回答: Blue"), "{output}");
    assert!(output.contains("Got your answer: Blue"), "{output}");
    assert!(!output.contains("Agent question"), "{output}");
    assert!(!output.contains("Select one:"), "{output}");
    assert!(!output.contains("Left/Right move"), "{output}");
    assert!(!output.contains("Answer: Blue"), "{output}");
    assert!(!output.contains("Answer sent"), "{output}");
    assert!(!output.contains("Sent to Agent"), "{output}");
    assert!(!output.contains("/answer"), "{output}");
    assert!(!output.contains("bash:"), "{output}");
    assert_no_migrated_english_ui_labels(&output, QUESTION_ZH_FORBIDDEN_UI);
}

#[test]
fn raw_cli_agent_question_answer_slash_is_ignored() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (b"?? ask question\n".to_vec(), Duration::ZERO),
            (b"/answer Blue\n".to_vec(), Duration::from_millis(1_200)),
            (b"\x1b[C\n".to_vec(), Duration::from_millis(300)),
            (b"exit\n".to_vec(), Duration::from_millis(200)),
        ],
    );

    assert!(output.contains("Agent question"), "{output}");
    assert!(
        !output.contains("Got your answer: /answer Blue"),
        "{output}"
    );
    assert!(output.contains("Got your answer: Blue"), "{output}");
}

#[test]
fn raw_cli_agent_free_text_question_echoes_input_before_submit() {
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$ ", b"?? ask free question\n"),
            ("Tell me the branch name to inspect", b"feature/x"),
            ("Answer: feature/x", b"\x03"),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains("Agent question"), "{output}");
    assert!(
        output.contains("Tell me the branch name to inspect"),
        "{output}"
    );
    assert!(output.contains("Answer: feature/x"), "{output}");
    assert!(!output.contains("Got your answer: feature/x"), "{output}");
}

#[test]
fn raw_cli_agent_free_text_empty_submit_stays_on_card() {
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$", b"?? ask free question\n"),
            ("Type answer | Enter send", b"\n"),
            ("Please enter an answer", b"main\n"),
            ("Got your answer: main", b"exit\n"),
        ],
    );

    assert!(output.contains("Type your answer..."), "{output}");
    assert!(output.contains("Please enter an answer"), "{output}");
    assert!(!output.contains("No pending question"), "{output}");
    assert!(output.contains("Answer: main"), "{output}");
    assert!(output.contains("Got your answer: main"), "{output}");
    assert!(
        !output.contains("Got your answer: Type your answer"),
        "{output}"
    );
}

#[test]
fn raw_cli_agent_question_no_color_keeps_box_placeholder() {
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[("NO_COLOR", "1"), ("TERM", "xterm-256color")],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$", b"?? ask free question\n"),
            ("Type answer | Enter send", b"\n"),
            ("Please enter an answer", b"main\n"),
            ("Got your answer: main", b"exit\n"),
        ],
    );

    assert!(output.contains("╭ Agent question"), "{output}");
    assert!(output.contains("Type your answer..."), "{output}");
    assert!(output.contains("Please enter an answer"), "{output}");
    assert!(!output.contains("\u{1b}[3"), "{output}");
    assert!(output.contains("Got your answer: main"), "{output}");
}

#[test]
fn raw_cli_agent_multiple_empty_submit_stays_on_card() {
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$", b"?? ask multi question\n"),
            ("Space toggle | Enter send", b"\n"),
            ("Select at least one option or enter an answer", b" \n"),
            ("Got your answer: Lint", b"exit\n"),
        ],
    );

    assert!(
        output.contains("Select at least one option or enter an answer"),
        "{output}"
    );
    assert!(!output.contains("No pending question"), "{output}");
    assert!(output.contains("Answer: Lint"), "{output}");
    assert!(output.contains("Got your answer: Lint"), "{output}");
}

#[test]
fn raw_cli_agent_multiple_accepts_custom_only_answer() {
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$", b"?? ask multi question\n"),
            ("Space toggle | Enter send", b"123\n"),
            ("Got your answer: 123", b"exit\n"),
        ],
    );

    assert!(output.contains("Answer: 123"), "{output}");
    assert!(output.contains("Got your answer: 123"), "{output}");
    assert!(!output.contains("Choose a valid answer"), "{output}");
}

#[test]
fn raw_cli_agent_question_ctrl_c_cancels_card_without_answer_turn() {
    // Keep delayed input: after Ctrl-C cancels the card the prompt repaints
    // before input ownership returns to the shell, so a marker-gated echo
    // right after cancellation can be silently dropped (ownership gap).
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (b"?? ask question\n".to_vec(), Duration::ZERO),
            (vec![0x03], Duration::from_millis(1_500)),
            (
                b"echo after-question-cancel\nexit\n".to_vec(),
                Duration::from_millis(200),
            ),
        ],
    );

    assert!(output.contains("Agent question"), "{output}");
    assert!(output.contains("after-question-cancel"), "{output}");
    assert!(!output.contains("Got your answer:"), "{output}");
    assert!(!output.contains("Answer:"), "{output}");
    assert!(!output.contains("bash:"), "{output}");
}

#[test]
fn raw_cli_zsh_question_card_capture_does_not_leak_to_shell() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "zsh"],
        &[("COSH_SHELL_ISOLATED", "1"), ("TERM", "xterm-256color")],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$", b"?? ask question\n"),
            ("Left/Right move | Enter send", b"\x1b[C\n"),
            ("Got your answer: Blue", b"echo after-zsh-question\n"),
            ("after-zsh-question", b"exit\n"),
        ],
    );

    assert!(output.contains("Agent question"), "{output}");
    assert!(
        output.contains("Choose a color for the next step"),
        "{output}"
    );
    assert!(output.contains("Answer: Blue"), "{output}");
    assert!(output.contains("Got your answer: Blue"), "{output}");
    assert!(output.contains("after-zsh-question"), "{output}");
    assert!(!output.contains("zsh: command not found"), "{output}");
    assert!(
        !output.contains("zsh: no such file or directory"),
        "{output}"
    );
    assert!(!output.contains("^[[C"), "{output}");
    assert!(!output.contains("/answer"), "{output}");
    assert!(!output.contains("\x1b]1337;COSH;"), "{output}");
}

#[test]
fn raw_cli_agent_question_answer_drops_stale_held_text() {
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$ ", b"?? stream stale question\n"),
            ("Left/Right move", b"\x1b[B\n"),
            ("Got your answer: Blue", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains("Agent question"), "{output}");
    assert!(!output.contains("Agent question q-1"), "{output}");
    assert!(output.contains("Answer: Blue"), "{output}");
    assert!(output.contains("Got your answer: Blue"), "{output}");
    assert!(
        !output.contains("STALE QUESTION TEXT SHOULD NOT RENDER"),
        "{output}"
    );
    assert!(!output.contains("Answer sent"), "{output}");
    assert!(!output.contains("Sent to Agent"), "{output}");
    assert!(!output.contains("bash:"), "{output}");
}

#[test]
fn raw_cli_agent_question_accepts_multiple_card_answers() {
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$ ", b"?? ask multi question\n"),
            ("Space toggle", b" \t \n"),
            ("Got your answer: Lint, Unit tests", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains("Agent question"), "{output}");
    assert!(output.contains("Choose checks to run"), "{output}");
    assert!(output.contains("Select one or more:"), "{output}");
    assert!(output.contains("[x] [1] Lint"), "{output}");
    assert!(output.contains("[x] [2] Unit tests"), "{output}");
    assert!(output.contains("[ ] [3] Raw shell smoke"), "{output}");
    assert!(output.contains("Space toggle"), "{output}");
    assert!(output.contains("Answer: Lint, Unit tests"), "{output}");
    assert!(!output.contains("Answer sent"), "{output}");
    assert!(output.contains("Answer: Lint, Unit tests"), "{output}");
    assert!(!output.contains("Sent to Agent"), "{output}");
    assert!(!output.contains("no command ran"), "{output}");
    assert!(
        !output.contains("Continuing the same Agent session"),
        "{output}"
    );
    assert!(!output.contains("Question: Choose checks"), "{output}");
    assert!(
        output.contains("Got your answer: Lint, Unit tests"),
        "{output}"
    );
    assert!(!output.contains("/answer"), "{output}");
    assert!(!output.contains("bash:"), "{output}");
}

#[test]
fn raw_cli_agent_question_accepts_multiple_answers_with_custom_card_answer() {
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$ ", b"?? ask multi question\n"),
            ("Space toggle", b" \t\t\tDocs\n"),
            ("Got your answer: Lint, Docs", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains("Choose checks to run"), "{output}");
    assert!(output.contains("[x] [1] Lint"), "{output}");
    assert!(output.contains("> [4] Answer: Docs"), "{output}");
    assert!(output.contains("Answer: Lint, Docs"), "{output}");
    assert!(!output.contains("Answer sent"), "{output}");
    assert!(output.contains("Answer: Lint, Docs"), "{output}");
    assert!(!output.contains("Sent to Agent"), "{output}");
    assert!(!output.contains("no command ran"), "{output}");
    assert!(
        !output.contains("Continuing the same Agent session"),
        "{output}"
    );
    assert!(output.contains("Got your answer: Lint, Docs"), "{output}");
    assert!(!output.contains("/answer"), "{output}");
    assert!(!output.contains("bash:"), "{output}");
}

#[test]
fn raw_cli_agent_question_accepts_natural_language_answer() {
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$ ", b"?? ask question\n"),
            ("Left/Right move", "\u{7eff}\u{8272}\n".as_bytes()),
            ("Got your answer: \u{7eff}\u{8272}", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains("Agent question"), "{output}");
    assert!(!output.contains("Answer sent"), "{output}");
    assert!(output.contains("Answer: \u{7eff}\u{8272}"), "{output}");
    assert!(!output.contains("Sent to Agent"), "{output}");
    assert!(!output.contains("no command ran"), "{output}");
    assert!(
        !output.contains("Continuing the same Agent session"),
        "{output}"
    );
    assert!(!output.contains("Question: Choose a color"), "{output}");
    assert!(
        output.contains("Got your answer: \u{7eff}\u{8272}"),
        "{output}"
    );
    assert!(output.contains("Answer: \u{7eff}\u{8272}"), "{output}");
    assert!(!output.contains("/answer"), "{output}");
    assert!(!output.contains("bash:"), "{output}");
}

#[test]
fn raw_cli_agent_question_accepts_custom_card_answer() {
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$ ", b"?? ask question\n"),
            ("Left/Right move", "\t\t\t\u{7ea2}\u{8272}\n".as_bytes()),
            ("Got your answer: \u{7ea2}\u{8272}", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains("Agent question"), "{output}");
    assert!(!output.contains("Agent question q-1"), "{output}");
    assert!(
        output.contains("> [4] Answer: \u{7ea2}\u{8272}"),
        "{output}"
    );
    assert!(!output.contains("Answer sent"), "{output}");
    assert!(output.contains("Answer: \u{7ea2}\u{8272}"), "{output}");
    assert!(!output.contains("Sent to Agent"), "{output}");
    assert!(!output.contains("no command ran"), "{output}");
    assert!(
        !output.contains("Continuing the same Agent session"),
        "{output}"
    );
    assert!(
        output.contains("Got your answer: \u{7ea2}\u{8272}"),
        "{output}"
    );
    assert!(!output.contains("Question: Choose a color"), "{output}");
    assert!(!output.contains("/answer"), "{output}");
    assert!(!output.contains("bash:"), "{output}");
}

#[test]
fn raw_cli_agent_question_accepts_free_text_only_answer() {
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$ ", b"?? ask free question\n"),
            (
                "Tell me the branch name to inspect",
                "\u{7279}\u{6027}\u{5206}\u{652f}\n".as_bytes(),
            ),
            ("Got your answer: \u{7279}\u{6027}\u{5206}\u{652f}", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains("Agent question"), "{output}");
    assert!(!output.contains("Agent question q-1"), "{output}");
    assert!(
        output.contains("Tell me the branch name to inspect"),
        "{output}"
    );
    assert!(output.contains("Type answer"), "{output}");
    assert!(output.contains("Enter send"), "{output}");
    assert!(!output.contains("Type an answer"), "{output}");
    assert!(!output.contains("[1]"), "{output}");
    assert!(!output.contains("Answer sent"), "{output}");
    assert!(
        output.contains("Answer: \u{7279}\u{6027}\u{5206}\u{652f}"),
        "{output}"
    );
    assert!(!output.contains("Sent to Agent"), "{output}");
    assert!(!output.contains("no command ran"), "{output}");
    assert!(
        !output.contains("Continuing the same Agent session"),
        "{output}"
    );
    assert!(
        output.contains("Got your answer: \u{7279}\u{6027}\u{5206}\u{652f}"),
        "{output}"
    );
    assert!(!output.contains("/answer"), "{output}");
    assert!(!output.contains("bash:"), "{output}");
}
