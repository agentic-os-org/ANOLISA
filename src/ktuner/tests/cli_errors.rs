//! The README's error contract in the argument parser: "Errors go to stderr
//! as JSON." A usage error fires before any command runs, so it was the one
//! error channel left outside that contract — clap rendered plain text an
//! agent parsing stderr JSON could not read.
use std::process::{Command, Output};

fn ktuner(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ktuner"))
        .args(arguments)
        .output()
        .expect("run ktuner")
}

fn error_json(arguments: &[&str]) -> serde_json::Value {
    let out = ktuner(arguments);
    assert_eq!(
        out.status.code(),
        Some(2),
        "{arguments:?} must exit 2: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "{arguments:?} must not print on stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    serde_json::from_slice(&out.stderr).unwrap_or_else(|e| {
        panic!(
            "{arguments:?} must report its error as JSON on stderr, got {e}: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

#[test]
fn usage_errors_reach_stderr_as_json() {
    for arguments in [
        &["--definitely-not-a-flag"][..],
        &[][..],
        &["check", "--no-such-flag"][..],
        &["fix"][..],
        &["rollback", "--list", "extra"][..],
    ] {
        let error = error_json(arguments);
        assert!(
            error["error"].as_str().is_some_and(|s| !s.is_empty()),
            "{arguments:?} must carry a non-empty error string: {error}"
        );
    }
}

#[test]
fn help_and_version_keep_their_rendering_on_stdout() {
    for arguments in [&["--help"][..], &["--version"][..]] {
        let out = ktuner(arguments);
        assert_eq!(out.status.code(), Some(0), "{arguments:?}");
        assert!(
            !out.stdout.is_empty() && out.stderr.is_empty(),
            "{arguments:?} is not an error: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
