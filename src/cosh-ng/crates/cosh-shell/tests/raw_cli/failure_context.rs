use super::*;

#[test]
fn failure_context_without_interactive_question_has_no_execution_or_state_effects() {
    let home = tempfile::tempdir().unwrap();
    for args in [
        vec![
            "--context",
            "1234567890abcdef1234567890abcdef",
            "-c",
            "touch executed",
        ],
        vec![
            "--ask",
            "Analyze this failure",
            "--context",
            "1234567890abcdef1234567890abcdef",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_cosh-shell"))
            .args(args)
            .env("HOME", home.path())
            .current_dir(home.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
        assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
    }
}
