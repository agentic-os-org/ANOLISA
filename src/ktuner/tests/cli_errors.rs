//! The README's error contract in the argument parser: "Errors go to stderr
//! as JSON." A usage error fires before any command runs, so it was the one
//! error channel left outside that contract — clap rendered plain text an
//! agent parsing stderr JSON could not read.
use std::process::{Command, Output, Stdio};

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

/// An error reported to a stderr that cannot take the bytes keeps the
/// README's exit 2, and the command still does not panic: the body is the
/// detail, the status is the contract.
#[test]
fn an_unwritable_stderr_keeps_the_error_exit_code() {
    let Some(full) = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .ok()
    else {
        eprintln!("skipping: /dev/full is not available on this host");
        return;
    };

    for arguments in [
        // A usage error reported by the parser.
        &["--definitely-not-a-flag"][..],
        // A command error reported by the command itself.
        &["why", "this.is.not.a.parameter"][..],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_ktuner"))
            .args(arguments)
            .stderr(Stdio::from(full.try_clone().expect("dup /dev/full")))
            .output()
            .expect("run ktuner with a full stderr");
        assert_eq!(
            out.status.code(),
            Some(2),
            "{arguments:?} must keep the documented error code when stderr takes no bytes"
        );
    }
}

/// The error path a failed `--help` / `--version` write takes must keep its
/// status when stderr is unwritable too. Its diagnostic cannot be delivered,
/// but the status is what a caller reads, and the two sibling error paths
/// already answer this way.
#[test]
fn a_failed_help_write_keeps_its_status_with_a_full_stderr() {
    let (Some(stdout_full), Some(stderr_full)) = (
        std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .ok(),
        std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .ok(),
    ) else {
        eprintln!("skipping: /dev/full is not available on this host");
        return;
    };

    let out = Command::new(env!("CARGO_BIN_EXE_ktuner"))
        .arg("--help")
        .stdout(Stdio::from(stdout_full))
        .stderr(Stdio::from(stderr_full))
        .output()
        .expect("run ktuner --help with both streams full");
    assert_eq!(
        out.status.code(),
        Some(2),
        "a failed help write must keep the documented error code when nothing can be printed"
    );
}
