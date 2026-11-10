//! Empty-input regression across the four `scan-prompt` input channels.
//!
//! Exit 0 always carries a verdict, so an empty selection must fail the same
//! way whichever channel carries it: exit 1, a usage message on stderr, and
//! nothing on stdout that a caller could mistake for a scan result. Every case
//! below fails before any daemon traffic, so no daemon is required.

use std::process::{Command, Output, Stdio};

/// Runs the CLI with stdin closed, mirroring a caller redirecting /dev/null.
fn run_closed_stdin(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agent-sec-cli"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run the CLI")
}

/// Runs the CLI with *input* piped to stdin.
fn run_with_stdin(args: &[&str], input: &str) -> Output {
    use std::io::Write as _;

    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-sec-cli"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the CLI");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(input.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("collect the CLI output")
}

/// Asserts the shared empty-input contract: exit 1, empty stdout, and a usage
/// error naming the channel on stderr.
fn assert_empty_input_failure(output: &Output, channel: &str, hint: &str) {
    assert_eq!(output.status.code(), Some(1), "{channel}: exit code");
    assert!(
        output.stdout.is_empty(),
        "{channel}: stdout must stay empty, got {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("Error: "),
        "{channel}: usage error expected on stderr, got {stderr:?}"
    );
    assert!(
        stderr.contains(hint),
        "{channel}: stderr must name the channel, got {stderr:?}"
    );
}

#[test]
fn every_empty_input_channel_exits_1_without_a_verdict() {
    let directory = tempfile::tempdir().expect("create the temporary directory");
    let empty_file = directory.path().join("empty.txt");
    std::fs::write(&empty_file, "").expect("write the empty file");
    let empty_path = empty_file.to_str().expect("utf-8 temporary path");

    let cases = [
        ("blank --text", vec!["scan-prompt", "--text", ""], "--text"),
        (
            "whitespace --text",
            vec!["scan-prompt", "--text", "   "],
            "--text",
        ),
        (
            "empty --input file",
            vec!["scan-prompt", "--input", empty_path],
            "File is empty",
        ),
        (
            "empty stdin",
            vec!["scan-prompt"],
            "No input received from stdin",
        ),
    ];
    for (channel, args, hint) in cases {
        assert_empty_input_failure(&run_closed_stdin(&args), channel, hint);
    }

    // A blank --text keeps its precedence over the other channels: it is
    // rejected even when a populated --input file or stdin could have been
    // scanned instead of silently falling back to one of them.
    let populated_file = directory.path().join("populated.txt");
    std::fs::write(&populated_file, "hello\n").expect("write the populated file");
    let populated_path = populated_file.to_str().expect("utf-8 temporary path");
    assert_empty_input_failure(
        &run_closed_stdin(&["scan-prompt", "--text", " ", "--input", populated_path]),
        "blank --text with a populated --input",
        "--text",
    );
    assert_empty_input_failure(
        &run_with_stdin(&["scan-prompt", "--text", ""], "hello"),
        "blank --text with populated stdin",
        "--text",
    );
}
