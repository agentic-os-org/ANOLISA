//! Regression coverage for prompt replay after slash panels: a real empty
//! Enter must repaint a fresh prompt line instead of being deduplicated as a
//! replayed prompt echo (issue #1698).

use super::*;
use std::path::{Path, PathBuf};

const PROMPT: &str = "cosh-replay$ ";

/// Temporary shell HOME with a pinned prompt; removed on drop so repeated
/// runs do not leak `cosh-raw-cli-paste-*` trees under the temp dir.
struct TempReplayHome {
    root: PathBuf,
    home: String,
    inputrc: String,
}

impl TempReplayHome {
    fn new(label: &str, inputrc: &str) -> Self {
        let root = temp_shell_home(label);
        let inputrc_path = root.join(".inputrc");
        fs::write(&inputrc_path, inputrc).expect("write test INPUTRC");
        // Non-isolated shells source the user rc files; pin a deterministic
        // prompt and delay PROMPT_COMMAND so the accept-line bytes (CRLF +
        // bracketed-paste toggle) and the next prompt arrive in separate PTY
        // reads, like a real shell with a non-trivial PROMPT_COMMAND.
        fs::write(
            root.join(".bashrc"),
            format!("PS1='{PROMPT}'\nPROMPT_COMMAND='sleep 0.05'\n"),
        )
        .expect("write test bashrc");
        fs::write(
            root.join(".zshrc"),
            format!("PROMPT='{PROMPT}'\nprecmd() {{ sleep 0.05; }}\n"),
        )
        .expect("write test zshrc");
        Self {
            home: root.to_string_lossy().into_owned(),
            inputrc: inputrc_path.to_string_lossy().into_owned(),
            root,
        }
    }

    /// Seeds bash history so `Up` recalls a slash command through the bounded
    /// Readline submission guard instead of the raw candidate relay.
    fn seed_bash_history(&self, line: &str) {
        fs::write(
            self.root.join(".bashrc"),
            format!(
                "PS1='{PROMPT}'\nPROMPT_COMMAND='sleep 0.05'\n\
                 export HISTFILE=\"$HOME/.bash_history\"\n\
                 export HISTSIZE=1000\nshopt -s histappend\n"
            ),
        )
        .expect("write history-enabled bashrc");
        fs::write(self.root.join(".bash_history"), format!("{line}\n"))
            .expect("write seeded history");
    }

    fn enable_bash_history(&self) {
        fs::write(
            self.root.join(".bashrc"),
            format!(
                "PS1='{PROMPT}'\nPROMPT_COMMAND='sleep 0.05'\n\
                 export HISTFILE=\"$HOME/.bash_history\"\n\
                 export HISTSIZE=1000\nexport HISTFILESIZE=1000\nshopt -s histappend\n"
            ),
        )
        .expect("write history-enabled bashrc");
        fs::write(self.root.join(".bash_history"), "").expect("write empty history");
    }

    /// Emits hook output and then delays PROMPT_COMMAND long enough to outlast
    /// the idle-reconcile window before the PS1 paint.
    fn set_bash_prompt_command_delay(&self, seconds: &str) {
        fs::write(
            self.root.join(".bashrc"),
            format!(
                "PS1='{PROMPT}'\n\
                 PROMPT_COMMAND='printf prompt-hook-output; sleep {seconds}'\n"
            ),
        )
        .expect("write delayed-prompt bashrc");
    }
}

impl Drop for TempReplayHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Slice between the end of the skills panel and the sentinel echo, where the
/// empty-Enter response must appear.
fn between_panel_and_sentinel<'a>(output: &'a str, sentinel: &str) -> &'a str {
    between_marker_and_sentinel(output, "Skills", sentinel)
}

fn between_marker_and_sentinel<'a>(output: &'a str, marker: &str, sentinel: &str) -> &'a str {
    let panel = output.find(marker).expect("panel marker");
    let after_panel = &output[panel..];
    let sentinel_at = after_panel
        .find(sentinel)
        .map(|idx| panel + idx)
        .expect("sentinel echo");
    &output[panel..sentinel_at]
}

fn assert_no_prompt_run_on(output: &str, sentinel: &str) {
    assert_no_prompt_run_on_between(output, "Skills", sentinel);
}

fn assert_no_prompt_run_on_between(output: &str, marker: &str, sentinel: &str) {
    let normalized = strip_ansi_escape(between_marker_and_sentinel(output, marker, sentinel));
    for line in normalized.split(['\r', '\n']) {
        assert!(
            count_occurrences(line, PROMPT.trim_end()) <= 1,
            "two prompts written on one line: {line:?}\n{output:?}"
        );
    }
}

/// Asserts the empty Enter produced a fresh prompt line: two prompts after the
/// panel (the synthesized replay and the one bash paints after Enter), never
/// glued on one visible line, with a line break between them.
fn assert_empty_enter_repaints_prompt(output: &str, sentinel: &str) {
    let between = between_panel_and_sentinel(output, sentinel);
    let normalized = strip_ansi_escape(between);
    let prompt_positions = normalized
        .match_indices(PROMPT.trim_end())
        .map(|(idx, _)| idx)
        .collect::<Vec<_>>();
    assert!(
        prompt_positions.len() >= 2,
        "empty Enter did not repaint a prompt after the panel\n{output:?}"
    );
    for pair in prompt_positions.windows(2) {
        assert!(
            normalized[pair[0]..pair[1]].contains('\n'),
            "empty Enter CRLF was swallowed; prompts run on together\n{output:?}"
        );
    }
}

#[test]
fn raw_cli_bash_bracketed_paste_empty_enter_after_slash_panel_repaints_prompt() {
    let home = TempReplayHome::new("paste-on", "set enable-bracketed-paste on\n");
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("HOME", home.home.as_str()),
            ("INPUTRC", home.inputrc.as_str()),
            // The isolated-mode fallback writes the prompt inline and skips
            // the RestorePrompt replay path this regression must exercise.
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        vec![
            // Let the initial prompt render before typing the slash command,
            // as a real user would.
            (
                b"/skills disable xlsx\r".to_vec(),
                Duration::from_millis(600),
            ),
            // Wait for the panel and the synthesized prompt replay to finish
            // before sending the lone empty Enter.
            (b"\r".to_vec(), Duration::from_millis(800)),
            (
                b"echo replay-sentinel-on\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(300)),
        ],
    );

    // Raw bytes: readline emits ESC[?2004l when the empty Enter is accepted;
    // the toggle must reach the outer terminal instead of being consumed as a
    // replay separator.
    let between = between_panel_and_sentinel(&output, "replay-sentinel-on");
    assert!(
        count_occurrences(between, "\u{1b}[?2004l") >= 1,
        "bracketed paste disable of the empty Enter was swallowed\n{output:?}"
    );
    assert_empty_enter_repaints_prompt(&output, "replay-sentinel-on");
    assert!(output.contains("replay-sentinel-on"), "{output}");
}

#[test]
fn raw_cli_bash_bracketed_paste_empty_enter_within_delay_window_is_not_swallowed() {
    let home = TempReplayHome::new("paste-delay", "set enable-bracketed-paste on\n");
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("HOME", home.home.as_str()),
            ("INPUTRC", home.inputrc.as_str()),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        vec![
            // The empty Enter rides in the same chunk as the slash command, so
            // it is relayed to bash while the panel still holds shell output.
            (
                b"/skills disable xlsx\r\r".to_vec(),
                Duration::from_millis(600),
            ),
            (
                b"echo replay-sentinel-delay\n".to_vec(),
                Duration::from_millis(800),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(300)),
        ],
    );

    let between = between_panel_and_sentinel(&output, "replay-sentinel-delay");
    assert!(
        count_occurrences(between, "\u{1b}[?2004l") >= 1,
        "bracketed paste disable of the early empty Enter was swallowed\n{output:?}"
    );
    assert!(
        between.contains("\r\r\n") || between.contains("\r\n"),
        "early empty Enter CRLF was swallowed\n{output:?}"
    );
    assert_no_prompt_run_on(&output, "replay-sentinel-delay");
    assert!(output.contains("replay-sentinel-delay"), "{output}");
}

#[test]
fn raw_cli_bash_recalled_slash_with_same_chunk_empty_enter_is_not_swallowed() {
    let home = TempReplayHome::new("paste-recall", "set enable-bracketed-paste on\n");
    // The recalled line reaches Readline itself, so the intercept travels the
    // bounded submission-guard path; the trailing empty Enter shares the same
    // PTY write and must still reach Bash.
    home.seed_bash_history("/skills disable xlsx");
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("HOME", home.home.as_str()),
            ("INPUTRC", home.inputrc.as_str()),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        vec![
            // Up-arrow recalls the seeded slash command from bash history.
            (b"\x1b[A".to_vec(), Duration::from_millis(600)),
            // Submit it and the empty Enter in one chunk (one relay write).
            (b"\r\r".to_vec(), Duration::from_millis(300)),
            (
                b"echo replay-sentinel-recall\n".to_vec(),
                Duration::from_millis(1000),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(300)),
        ],
    );

    assert!(
        !output.contains("bash: /skills"),
        "recalled slash leaked to bash\n{output:?}"
    );
    let between = between_panel_and_sentinel(&output, "replay-sentinel-recall");
    assert!(
        count_occurrences(between, "\u{1b}[?2004l") >= 1,
        "bracketed paste disable of the empty Enter was swallowed\n{output:?}"
    );
    assert!(
        between.contains("\r\r\n") || between.contains("\r\n"),
        "empty Enter CRLF after the recalled slash was swallowed\n{output:?}"
    );
    assert_no_prompt_run_on(&output, "replay-sentinel-recall");
    assert!(output.contains("replay-sentinel-recall"), "{output}");
}

#[test]
fn raw_cli_bash_recalled_ordinary_command_remains_exact_without_prompt_run_on() {
    let home = TempReplayHome::new("ordinary-recall", "set enable-bracketed-paste on\n");
    let execution_marker = home.root.join(".ordinary-recall-count");
    home.seed_bash_history("printf x >> \"$HOME/.ordinary-recall-count\"");
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("HOME", home.home.as_str()),
            ("INPUTRC", home.inputrc.as_str()),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        vec![
            (b"\x1b[A".to_vec(), Duration::from_millis(600)),
            (b"\r".to_vec(), Duration::from_millis(300)),
            (
                b"echo replay-sentinel-ordinary-recall\n".to_vec(),
                Duration::from_millis(600),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(300)),
        ],
    );

    assert_eq!(
        fs::read(execution_marker).expect("recalled ordinary command executed"),
        b"x",
        "recalled ordinary command must execute exactly once\n{output:?}"
    );
    assert_no_prompt_run_on_between(&output, PROMPT, "replay-sentinel-ordinary-recall");
    assert!(
        output.contains("replay-sentinel-ordinary-recall"),
        "{output}"
    );
}

#[test]
fn raw_cli_bash_recalled_natural_language_reaches_provider_after_edit() {
    if !bash_supports_command_not_found_handler() {
        return;
    }

    let home = TempReplayHome::new("natural-language-recall", "set enable-bracketed-paste on\n");
    home.seed_bash_history("echo history-recall-seed");
    let prompt = "你好你是谁";
    let response = format!("Received shell prompt request: {prompt}");
    let edited_response = format!("Received shell prompt request: {prompt}?");
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", home.home.as_str()),
            ("INPUTRC", home.inputrc.as_str()),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            (PROMPT, format!("{prompt}\r").as_bytes()),
            (response.as_str(), b""),
            (PROMPT, b"\x1b[A"),
            (prompt, b"\r"),
            (response.as_str(), b""),
            (PROMPT, b"\x1b[A"),
            (prompt, b"?\r"),
            (edited_response.as_str(), b""),
            (PROMPT, b"echo history-recall-control\r"),
            ("history-recall-control", b"exit\r"),
        ],
    );

    assert_eq!(
        count_occurrences(&output, &format!("{response} ")),
        2,
        "{output}"
    );
    assert_eq!(
        count_occurrences(&output, &format!("{edited_response} ")),
        1,
        "{output}"
    );
    assert!(
        !output.contains(&format!("Received shell prompt request:  {prompt}")),
        "synthetic privacy space reached provider: {output}"
    );
    assert!(!output.contains("command not found"), "{output}");
    for internal in ["__cosh_slash_guard__", "_COSH_HANDOFF", "1337;COSH;"] {
        assert!(!output.contains(internal), "{internal}: {output}");
    }
}

#[test]
fn raw_cli_bash_recall_walks_distinct_natural_language_history_entries() {
    if !bash_supports_command_not_found_handler() {
        return;
    }

    let home = TempReplayHome::new("natural-language-history-walk", "");
    let first = "你好1";
    let second = "你好2";
    let third = "你好3";
    let first_response = format!("Received shell prompt request: {first}");
    let second_response = format!("Received shell prompt request: {second}");
    let third_response = format!("Received shell prompt request: {third}");
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", home.home.as_str()),
            ("INPUTRC", home.inputrc.as_str()),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            (PROMPT, format!("{first}\r").as_bytes()),
            (first_response.as_str(), b""),
            (PROMPT, format!("{second}\r").as_bytes()),
            (second_response.as_str(), b""),
            (PROMPT, format!("{third}\r").as_bytes()),
            (third_response.as_str(), b""),
            (PROMPT, b"\x1b[A"),
            (third, b"\x1b[A"),
            ("\u{8}2", b"\x1b[A"),
            ("\u{8}1", b"\x1b[B"),
            ("\u{8}2", b"\x1b[B"),
            ("\u{8}3", b"\x15exit\r"),
        ],
    );
    for response in [first_response, second_response, third_response] {
        assert_eq!(count_occurrences(&output, &response), 1, "{output}");
    }
    assert!(!output.contains("command not found"), "{output}");
}

#[test]
fn raw_cli_bash_dirty_safe_natural_language_stays_recallable() {
    if !bash_supports_command_not_found_handler() {
        return;
    }

    let home = TempReplayHome::new("natural-language-dirty-history", "");
    home.enable_bash_history();
    let first = "你好1";
    let second = "你好2";
    let third = "你好3";
    let first_response = format!("Received shell prompt request: {first}");
    let second_response = format!("Received shell prompt request: {second}");
    let third_response = format!("Received shell prompt request: {third}");
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", home.home.as_str()),
            ("INPUTRC", home.inputrc.as_str()),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_RECOMMENDATIONS_ENABLED", "0"),
        ],
        vec![
            (b"\x1b[H".to_vec(), Duration::from_millis(600)),
            (
                format!("{first}\r").into_bytes(),
                Duration::from_millis(100),
            ),
            (
                format!("{second}\r").into_bytes(),
                Duration::from_millis(1_200),
            ),
            (b"\x1b[H".to_vec(), Duration::from_millis(1_200)),
            (
                format!("{third}\r").into_bytes(),
                Duration::from_millis(100),
            ),
            (b"\x1b[A".to_vec(), Duration::from_millis(1_200)),
            (b"\x1b[A".to_vec(), Duration::from_millis(200)),
            (b"\x1b[A".to_vec(), Duration::from_millis(200)),
            (b"\x1b[A".to_vec(), Duration::from_millis(200)),
            (b"\x1b[A".to_vec(), Duration::from_millis(200)),
            (b"\x15exit\r".to_vec(), Duration::from_millis(200)),
        ],
    );
    for response in [first_response, second_response, third_response] {
        assert_eq!(count_occurrences(&output, &response), 1, "{output}");
    }
    assert!(!output.contains("command not found"), "{output}");
    assert!(
        output.contains(&format!("{PROMPT}{third}\x082\x081\x07\x07")),
        "dirty history did not recall 3 -> 2 -> 1 and stop at the oldest entry\n{output:?}"
    );
    let history = fs::read_to_string(home.root.join(".bash_history")).expect("history file");
    let recalled = history
        .lines()
        .filter(|line| matches!(*line, "你好1" | "你好2" | "你好3"))
        .collect::<Vec<_>>();
    assert_eq!(recalled, [first, second, third], "{history:?}");
}

#[test]
fn raw_cli_bash_recovered_history_keeps_private_inputs_excluded() {
    if !bash_supports_command_not_found_handler() {
        return;
    }

    let home = TempReplayHome::new("recovered-history-privacy", "");
    home.enable_bash_history();
    let secret = "你好 token=TEST_ONLY_RECOVERY_SECRET";
    let quoted_assignment = "你好 \"token\" = TEST_ONLY_RECOVERY_SECRET";
    let aws_assignment = "你好 AWS_SECRET_ACCESS_KEY=TEST_ONLY_RECOVERY_SECRET";
    let provider_assignment = "你好 AWS_ACCESS_KEY_ID = TEST_ONLY_RECOVERY_SECRET";
    let jwt = "你好 eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.signature";
    let leading = " 你好-leading";
    let control = "你好-control";
    run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", home.home.as_str()),
            ("INPUTRC", home.inputrc.as_str()),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_RECOMMENDATIONS_ENABLED", "0"),
        ],
        vec![
            (b"\x1b[H".to_vec(), Duration::from_millis(600)),
            (
                format!("{secret}\r").into_bytes(),
                Duration::from_millis(100),
            ),
            (b"\x1b[H".to_vec(), Duration::from_millis(1_200)),
            (
                format!("{quoted_assignment}\r").into_bytes(),
                Duration::from_millis(100),
            ),
            (b"\x1b[H".to_vec(), Duration::from_millis(1_200)),
            (
                format!("{aws_assignment}\r").into_bytes(),
                Duration::from_millis(100),
            ),
            (b"\x1b[H".to_vec(), Duration::from_millis(1_200)),
            (
                format!("{provider_assignment}\r").into_bytes(),
                Duration::from_millis(100),
            ),
            (b"\x1b[H".to_vec(), Duration::from_millis(1_200)),
            (format!("{jwt}\r").into_bytes(), Duration::from_millis(100)),
            (
                format!("{leading}\r").into_bytes(),
                Duration::from_millis(1_200),
            ),
            (
                format!("{control}\r").into_bytes(),
                Duration::from_millis(1_200),
            ),
            (b"exit\r".to_vec(), Duration::from_millis(1_200)),
        ],
    );

    let history = fs::read_to_string(home.root.join(".bash_history")).expect("history file");
    assert!(!history.contains(secret), "{history:?}");
    assert!(!history.contains(quoted_assignment), "{history:?}");
    assert!(!history.contains(aws_assignment), "{history:?}");
    assert!(!history.contains(provider_assignment), "{history:?}");
    assert!(!history.contains(jwt), "{history:?}");
    assert!(!history.lines().any(|line| line == leading), "{history:?}");
    assert_eq!(
        history.lines().filter(|line| *line == control).count(),
        1,
        "{history:?}"
    );
}

#[test]
fn raw_cli_bash_preserves_user_leading_whitespace_for_provider() {
    if !bash_supports_command_not_found_handler() {
        return;
    }

    for (label, submission, expected_input) in [
        ("one-space", " 你好\r", " 你好"),
        ("three-spaces", "   你好\r", "   你好"),
        ("completion-tab-control", "\t你好\r", "你好"),
        ("one-space-punctuation", " 你好?\r", " 你好?"),
        ("three-spaces-punctuation", "   你好?\r", "   你好?"),
    ] {
        let home = TempReplayHome::new(label, "set enable-bracketed-paste on\n");
        let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
            "fake",
            &["--shell", "bash"],
            &[
                ("HOME", home.home.as_str()),
                ("INPUTRC", home.inputrc.as_str()),
                ("COSH_SHELL_ISOLATED", "0"),
            ],
            Path::new(env!("CARGO_MANIFEST_DIR")),
            &[
                (PROMPT, submission.as_bytes()),
                ("Received shell prompt request:", b""),
                (PROMPT, b"exit\r"),
            ],
        );

        let expected = format!("Received shell prompt request: {expected_input}");
        assert_eq!(
            count_occurrences(&output, &expected),
            1,
            "{label}: {output}"
        );
        assert!(!output.contains("command not found"), "{label}: {output}");
        for internal in ["__cosh_slash_guard__", "_COSH_HANDOFF", "1337;COSH;"] {
            assert!(!output.contains(internal), "{label}/{internal}: {output}");
        }
    }
}

#[test]
fn raw_cli_bash_bracketed_literal_tab_submits_when_closer_and_enter_share_batch() {
    if !bash_supports_command_not_found_handler() {
        return;
    }

    let mut same_batch = b"\x1b[200~\t".to_vec();
    same_batch.extend_from_slice("你好".as_bytes());
    same_batch.extend_from_slice(b"\x1b[201~\r");
    assert_literal_leading_tab_case(
        "literal-tab-paste-same-batch",
        &[
            (PROMPT, same_batch.as_slice()),
            ("Received shell prompt request:", b""),
        ],
        "\t你好",
    );
}

#[test]
fn raw_cli_bash_bracketed_literal_tab_submits_when_enter_follows_closer() {
    if !bash_supports_command_not_found_handler() {
        return;
    }

    let mut paste_only = b"\x1b[200~\t".to_vec();
    paste_only.extend_from_slice("你好".as_bytes());
    paste_only.extend_from_slice(b"\x1b[201~");
    assert_literal_leading_tab_case(
        "literal-tab-paste-split-enter",
        &[
            (PROMPT, paste_only.as_slice()),
            ("你好", b"\r"),
            ("Received shell prompt request:", b""),
        ],
        "\t你好",
    );
}

#[test]
fn raw_cli_bash_quoted_literal_tab_reaches_provider_on_first_enter() {
    if !bash_supports_command_not_found_handler() {
        return;
    }

    let mut quoted_tab = b"\x16\t".to_vec();
    quoted_tab.extend_from_slice("你好?".as_bytes());
    quoted_tab.push(b'\r');
    assert_literal_leading_tab_case(
        "literal-tab-quoted-insert",
        &[
            (PROMPT, quoted_tab.as_slice()),
            ("Received shell prompt request:", b""),
        ],
        "\t你好?",
    );
}

#[test]
fn raw_cli_bash_split_quoted_tab_survives_a_prior_provider_card() {
    if !bash_supports_command_not_found_handler() {
        return;
    }

    let home = TempReplayHome::new(
        "literal-tab-after-provider",
        "set enable-bracketed-paste on\n",
    );
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", home.home.as_str()),
            ("INPUTRC", home.inputrc.as_str()),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_ANALYSIS_MODE", "smart"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
        ],
        vec![
            ("你好\r".as_bytes().to_vec(), Duration::from_millis(600)),
            (b"\x16".to_vec(), Duration::from_millis(1_200)),
            (b"\t".to_vec(), Duration::from_millis(200)),
            ("你好\r".as_bytes().to_vec(), Duration::from_millis(200)),
            (
                b"echo stateful-tab-control\r".to_vec(),
                Duration::from_millis(1_500),
            ),
            (b"exit\r".to_vec(), Duration::from_millis(500)),
        ],
    );

    assert_eq!(
        count_occurrences(&output, "Received shell prompt request: \t你好"),
        1,
        "{output}"
    );
    assert!(!output.contains("command not found"), "{output}");
    assert!(!output.contains("^V"), "{output}");
    assert!(output.contains("stateful-tab-control"), "{output}");
}

fn assert_literal_leading_tab_case(label: &str, input: &[(&str, &[u8])], expected_input: &str) {
    let response = format!("Received shell prompt request: {expected_input}");
    let home = TempReplayHome::new(label, "set enable-bracketed-paste on\n");
    let mut steps = input.to_vec();
    steps.push((PROMPT, b"exit\r"));
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", home.home.as_str()),
            ("INPUTRC", home.inputrc.as_str()),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &steps,
    );

    assert_eq!(
        count_occurrences(&output, &response),
        1,
        "{label}: {output}"
    );
    assert!(!output.contains("command not found"), "{label}: {output}");
    for internal in ["__cosh_slash_guard__", "_COSH_HANDOFF", "1337;COSH;"] {
        assert!(!output.contains(internal), "{label}/{internal}: {output}");
    }
}

#[test]
fn raw_cli_bash_typeahead_empty_enter_during_failing_command_survives_prompt_restore() {
    let home = TempReplayHome::new("paste-typeahead", "set enable-bracketed-paste on\n");
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("HOME", home.home.as_str()),
            ("INPUTRC", home.inputrc.as_str()),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_ANALYSIS_MODE", "auto"),
        ],
        vec![
            // A slightly delayed failing command that triggers the
            // failed-command prompt restore flow.
            (
                b"sh -c 'sleep 0.6; ls /nonexistent-replay-typeahead'\n".to_vec(),
                Duration::from_millis(600),
            ),
            // Empty Enter typed ahead while the command is still running: its
            // write event is drained before the command's precmd arrives.
            (b"\r".to_vec(), Duration::from_millis(300)),
            (
                b"echo replay-sentinel-typeahead\n".to_vec(),
                Duration::from_millis(2000),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(300)),
        ],
    );

    // The failed-command flow restores the prompt after the command
    // completes; that restore must not swallow the queued empty Enter's
    // response.
    let between = between_marker_and_sentinel(
        &output,
        "No such file or directory",
        "replay-sentinel-typeahead",
    );
    assert!(
        count_occurrences(between, "\u{1b}[?2004l") >= 1,
        "bracketed paste disable of the typeahead empty Enter was swallowed\n{output:?}"
    );
    assert!(
        between.contains("\r\r\n") || between.contains("\r\n"),
        "typeahead empty Enter CRLF was swallowed\n{output:?}"
    );
    let normalized = strip_ansi_escape(between);
    assert!(
        normalized.contains(PROMPT.trim_end()),
        "typeahead empty Enter did not repaint a prompt\n{output:?}"
    );
    for line in normalized.split(['\r', '\n']) {
        assert!(
            count_occurrences(line, PROMPT.trim_end()) <= 1,
            "two prompts written on one line: {line:?}\n{output:?}"
        );
    }
    assert!(output.contains("replay-sentinel-typeahead"), "{output}");
}

#[test]
fn raw_cli_bash_ctrl_o_submission_with_typeahead_empty_enter_is_not_swallowed() {
    let home = TempReplayHome::new("paste-ctrl-o", "set enable-bracketed-paste on\n");
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("HOME", home.home.as_str()),
            ("INPUTRC", home.inputrc.as_str()),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_ANALYSIS_MODE", "auto"),
        ],
        vec![
            // Submit a delayed failing command with Ctrl-O
            // (operate-and-get-next), bash's default non-Enter accept-line
            // binding, so the submission carries no CR/LF at all.
            (
                b"sh -c 'sleep 0.6; ls /nonexistent-replay-ctrl-o'\x0f".to_vec(),
                Duration::from_millis(600),
            ),
            // Empty Enter typed ahead while the command is still running.
            (b"\r".to_vec(), Duration::from_millis(300)),
            (
                b"echo replay-sentinel-ctrl-o\n".to_vec(),
                Duration::from_millis(2000),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(300)),
        ],
    );

    // The prompt restore after the failed command must not treat the
    // Ctrl-O-submitted command's precmd as acknowledging the queued Enter.
    let between = between_marker_and_sentinel(
        &output,
        "No such file or directory",
        "replay-sentinel-ctrl-o",
    );
    assert!(
        count_occurrences(between, "\u{1b}[?2004l") >= 1,
        "bracketed paste disable of the typeahead empty Enter was swallowed\n{output:?}"
    );
    assert!(
        between.contains("\r\r\n") || between.contains("\r\n"),
        "typeahead empty Enter CRLF was swallowed\n{output:?}"
    );
    let normalized = strip_ansi_escape(between);
    assert!(
        normalized.contains(PROMPT.trim_end()),
        "typeahead empty Enter did not repaint a prompt\n{output:?}"
    );
    for line in normalized.split(['\r', '\n']) {
        assert!(
            count_occurrences(line, PROMPT.trim_end()) <= 1,
            "two prompts written on one line: {line:?}\n{output:?}"
        );
    }
    assert!(output.contains("replay-sentinel-ctrl-o"), "{output}");
}

#[test]
fn raw_cli_bash_slow_prompt_command_does_not_write_off_queued_enter() {
    let home = TempReplayHome::new("paste-slow-precmd", "set enable-bracketed-paste on\n");
    // The precmd marker fires before the user's PROMPT_COMMAND body and the
    // PS1 paint. The body first emits output (which must not count as a
    // painted prompt), then stays silent for 500ms — far past the 200ms
    // idle-reconcile window while the queued Enter is still unconsumed by
    // readline. The ledger must survive and the Enter response must pass.
    home.set_bash_prompt_command_delay("0.5");
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("HOME", home.home.as_str()),
            ("INPUTRC", home.inputrc.as_str()),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_ANALYSIS_MODE", "auto"),
        ],
        vec![
            (
                b"sh -c 'sleep 0.6; ls /nonexistent-replay-slow-precmd'\n".to_vec(),
                Duration::from_millis(900),
            ),
            // Empty Enter typed ahead while the command still runs; it stays
            // queued through the whole PROMPT_COMMAND delay.
            (b"\r".to_vec(), Duration::from_millis(300)),
            (
                b"echo replay-sentinel-slow-precmd\n".to_vec(),
                Duration::from_millis(3000),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(300)),
        ],
    );

    let between = between_marker_and_sentinel(
        &output,
        "No such file or directory",
        "replay-sentinel-slow-precmd",
    );
    assert!(
        count_occurrences(between, "\u{1b}[?2004l") >= 1,
        "bracketed paste disable of the queued Enter was swallowed during \
         a slow PROMPT_COMMAND\n{output:?}"
    );
    assert!(
        between.contains("\r\r\n") || between.contains("\r\n"),
        "queued Enter CRLF was swallowed during a slow PROMPT_COMMAND\n{output:?}"
    );
    let normalized = strip_ansi_escape(between);
    assert!(
        normalized.contains(PROMPT.trim_end()),
        "queued Enter did not repaint a prompt after the slow PROMPT_COMMAND\n{output:?}"
    );
    for line in normalized.split(['\r', '\n']) {
        assert!(
            count_occurrences(line, PROMPT.trim_end()) <= 1,
            "two prompts written on one line: {line:?}\n{output:?}"
        );
    }
    assert!(output.contains("replay-sentinel-slow-precmd"), "{output}");
}

#[test]
fn raw_cli_bash_replay_dedup_recovers_after_foreground_program_consumed_enter() {
    let home = TempReplayHome::new("paste-read", "set enable-bracketed-paste on\n");
    // A Rust-owned slash panel after a foreground program verifies that the
    // orphaned submission ledger was reconciled before prompt replay arms.
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("HOME", home.home.as_str()),
            ("INPUTRC", home.inputrc.as_str()),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        vec![
            // A foreground `read` consumes the second Enter itself: the
            // submission ledger would otherwise stay off by one forever.
            (b"read value\r".to_vec(), Duration::from_millis(600)),
            (b"hello\r".to_vec(), Duration::from_millis(400)),
            // Idle at the prompt long enough for the write-off, then open a
            // Rust-owned slash panel: replay dedup must have recovered.
            (
                b"/skills disable xlsx\r".to_vec(),
                Duration::from_millis(800),
            ),
            (
                b"echo replay-sentinel-read\n".to_vec(),
                Duration::from_millis(1200),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(300)),
        ],
    );

    // With a stuck ledger the panel's prompt restore cannot arm, so the next
    // real prompt is painted alongside the synthesized replay.
    let between = between_panel_and_sentinel(&output, "replay-sentinel-read");
    let normalized = strip_ansi_escape(between);
    let prompts = count_occurrences(&normalized, PROMPT.trim_end());
    assert!(
        prompts <= 2,
        "replay dedup stayed disabled after a foreground `read`: \
         {prompts} prompts painted after the panel\n{output:?}"
    );
    assert!(output.contains("replay-sentinel-read"), "{output}");
}

#[test]
fn raw_cli_bash_bracketed_paste_off_empty_enter_after_slash_panel_repaints_prompt() {
    let home = TempReplayHome::new("paste-off", "set enable-bracketed-paste off\n");
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("HOME", home.home.as_str()),
            ("INPUTRC", home.inputrc.as_str()),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        vec![
            (
                b"/skills disable xlsx\r".to_vec(),
                Duration::from_millis(600),
            ),
            (b"\r".to_vec(), Duration::from_millis(800)),
            (
                b"echo replay-sentinel-off\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(300)),
        ],
    );

    let between = between_panel_and_sentinel(&output, "replay-sentinel-off");
    assert!(
        !between.contains("\u{1b}[?2004l"),
        "bracketed paste should be off in the baseline run\n{output:?}"
    );
    assert_empty_enter_repaints_prompt(&output, "replay-sentinel-off");
    assert!(output.contains("replay-sentinel-off"), "{output}");
}

#[test]
fn raw_cli_zsh_empty_enter_after_slash_panel_repaints_prompt() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let home = TempReplayHome::new("paste-zsh", "");
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "zsh"],
        &[("HOME", home.home.as_str()), ("COSH_SHELL_ISOLATED", "0")],
        vec![
            (
                b"/skills disable xlsx\r".to_vec(),
                Duration::from_millis(600),
            ),
            (b"\r".to_vec(), Duration::from_millis(800)),
            (
                b"echo replay-sentinel-zsh\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(300)),
        ],
    );

    assert_no_prompt_run_on(&output, "replay-sentinel-zsh");
    assert!(output.contains("replay-sentinel-zsh"), "{output}");
    assert!(
        !output.contains("zsh: no such file or directory: /skills"),
        "{output}"
    );
}

/// Regression for issue #1811: after a bash slash command is intercepted, the
/// echoed command text must not be replayed again below the panel.
///
/// bash echoes user input before the DEBUG trap fires, so the display buffer
/// contains `prompt$ /skills detail\r\n` when the intercept marker arrives.
/// Without advancing `last_prompt_display_start` past that echo, RestorePrompt
/// would re-emit the command text on the line below the panel.
#[test]
fn raw_cli_bash_slash_intercept_does_not_replay_user_command_echo() {
    let home = TempReplayHome::new("intercept-echo", "set enable-bracketed-paste on\n");
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("HOME", home.home.as_str()),
            ("INPUTRC", home.inputrc.as_str()),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        vec![
            // Type and submit a slash command that renders a usage panel.
            (b"/skills detail\r".to_vec(), Duration::from_millis(600)),
            // Wait for the panel and any synthesized prompt replay to settle,
            // then run a sentinel command.
            (
                b"echo replay-sentinel-1811\n".to_vec(),
                Duration::from_millis(800),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(300)),
        ],
    );

    // The only prompt line that should carry the slash text is the original
    // user input; RestorePrompt must not duplicate it below the panel.
    let normalized = strip_ansi_escape(&output);
    let echoed_lines = normalized
        .lines()
        .filter(|line| line.starts_with(PROMPT.trim_end()) && line.contains("/skills detail"))
        .count();
    assert_eq!(
        echoed_lines, 1,
        "expected exactly one prompt line containing /skills detail (the user input); \
         RestorePrompt duplicated the echoed command\n{output:?}"
    );
    assert!(output.contains("replay-sentinel-1811"), "{output}");
}
