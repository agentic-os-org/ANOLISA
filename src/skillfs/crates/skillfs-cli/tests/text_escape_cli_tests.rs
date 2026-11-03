//! Control-character escaping in CLI text output.
//!
//! Directory names are adopted verbatim as skill names and printed by the
//! text arms of `list`/`validate`. Printed raw, an embedded newline
//! fabricates report lines (an `evil\n  Injected: trusted summary line`
//! directory forges a summary line) and ESC/OSC sequences are live terminal
//! commands (OSC 777 is a notification/title command). Text output must
//! escape those bytes; JSON output keeps serde's escaping and must not
//! change.

use std::path::Path;
use std::process::Command;

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_skillfs")
}

/// A source tree whose skill directory names embed the two attack payloads:
/// a raw newline (line fabrication) and an OSC 777 sequence (terminal
/// command).
fn hostile_tree(parent: &Path) -> std::path::PathBuf {
    let source = parent.join("inj");
    let newline_dir = source.join("evil\n  Injected: trusted summary line");
    std::fs::create_dir_all(&newline_dir).expect("create newline-named skill dir");
    std::fs::write(
        newline_dir.join("SKILL.md"),
        "---\ndescription: legit\n---\nbody\n",
    )
    .expect("write SKILL.md");
    let osc_dir = source.join("ansi\u{1b}]777;id\u{7}");
    std::fs::create_dir_all(&osc_dir).expect("create OSC-named skill dir");
    std::fs::write(osc_dir.join("SKILL.md"), "---\ndescription: d\n---\nb\n")
        .expect("write SKILL.md");
    source
}

fn fabricated_line_present(stdout: &str) -> bool {
    stdout
        .lines()
        .any(|line| line.trim() == "Injected: trusted summary line")
}

#[test]
fn list_text_output_escapes_control_characters_in_skill_names() {
    let holder = tempfile::tempdir().expect("holder tempdir");
    let source = hostile_tree(holder.path());

    let out = Command::new(bin_path())
        .args(["list", source.to_str().unwrap()])
        .output()
        .expect("invoke skillfs list");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "list should succeed, stdout={stdout}");

    // No fabricated standalone line from the embedded newline.
    assert!(
        !fabricated_line_present(&stdout),
        "a newline inside a skill name must not fabricate output lines: {stdout:?}"
    );
    // No raw ESC byte from the OSC-named skill.
    assert!(
        !out.stdout.contains(&0x1b),
        "raw ESC (OSC terminal command) must not reach stdout: {stdout:?}"
    );
    // The hostile name is still reported, in escaped form.
    assert!(
        stdout.contains("evil\\n"),
        "the newline-named skill must be listed with an escaped newline: {stdout:?}"
    );
    assert!(
        stdout.contains("\\x1b]777;id\\x07"),
        "the OSC-named skill must be listed with an escaped ESC: {stdout:?}"
    );
}

#[test]
fn validate_text_output_escapes_control_characters_in_skill_names() {
    let holder = tempfile::tempdir().expect("holder tempdir");
    let source = hostile_tree(holder.path());

    let out = Command::new(bin_path())
        .args(["validate", source.to_str().unwrap()])
        .output()
        .expect("invoke skillfs validate");
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !fabricated_line_present(&stdout),
        "a newline inside a skill name must not fabricate report lines: {stdout:?}"
    );
    assert!(
        !out.stdout.contains(&0x1b),
        "raw ESC (OSC terminal command) must not reach stdout: {stdout:?}"
    );
    assert!(
        stdout.contains("evil\\n"),
        "the newline-named skill must be reported with an escaped newline: {stdout:?}"
    );
}

#[test]
fn validate_json_output_is_unchanged_by_text_escaping() {
    let holder = tempfile::tempdir().expect("holder tempdir");
    let source = hostile_tree(holder.path());

    let out = Command::new(bin_path())
        .args(["validate", source.to_str().unwrap(), "--format", "json"])
        .output()
        .expect("invoke skillfs validate --format json");
    assert!(
        out.status.success(),
        "validate --format json should succeed, stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    // serde_json's own escaping stays in charge in JSON mode: the control
    // characters are serialized (as \n / \u001b), never stripped or
    // double-mangled by the text-mode escaper.
    assert!(
        stdout.contains("evil\\n  Injected: trusted summary line"),
        "JSON must keep serde's escaping of the newline in the name: {stdout:?}"
    );
    assert!(
        stdout.contains("\\u001b]777;id\\u0007"),
        "JSON must keep serde's escaping of the OSC sequence: {stdout:?}"
    );
    // Still one JSON object, not multi-line text output.
    assert!(
        stdout.trim_start().starts_with('{'),
        "JSON output must remain a JSON document: {stdout:?}"
    );
}
