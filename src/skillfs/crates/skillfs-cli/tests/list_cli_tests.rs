//! CLI list subcommand tests.
//!
//! Verifies that `skillfs list` renders parse status with the canonical
//! `status_str()` names plus a plain-text reason, never leaking the
//! `Debug` representation of `ParseStatus` into script-readable output.

use std::path::Path;
use std::process::Command;

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_skillfs")
}

/// Valid SKILL.md that parses as Ok.
const VALID_SKILL: &str = r#"---
name: good-skill
description: A valid skill
version: "1.0"
---
# Good Skill

This skill works correctly.
"#;

/// SKILL.md with invalid YAML frontmatter → ParseStatus::Error.
const ERROR_SKILL: &str = r#"---
name: [invalid yaml
  broken: {{{}
---
Body text.
"#;

/// SKILL.md with missing description → ParseStatus::Degraded.
const DEGRADED_SKILL: &str = r#"---
name: degraded-skill
---
"#;

fn create_skill_dir(parent: &Path, name: &str, content: &str) {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).expect("create skill dir");
    std::fs::write(dir.join("SKILL.md"), content).expect("write SKILL.md");
}

#[test]
fn list_status_lines_use_clean_names() {
    let source = tempfile::tempdir().expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);
    create_skill_dir(source.path(), "degraded-skill", DEGRADED_SKILL);
    create_skill_dir(source.path(), "bad-yaml", ERROR_SKILL);

    let out = Command::new(bin_path())
        .args(["list", source.path().to_str().unwrap()])
        .output()
        .expect("invoke skillfs list");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "list should succeed, stdout={stdout}");

    assert!(
        stdout.contains("Status: ok | enabled"),
        "healthy skill must render the canonical ok status, stdout={stdout}"
    );
    assert!(
        stdout.contains("Status: degraded (missing description) | enabled"),
        "degraded skill must render status plus a plain-text reason, stdout={stdout}"
    );
    assert!(
        stdout.contains("Status: error (invalid YAML:"),
        "failed skill must render the canonical error status with reason, stdout={stdout}"
    );
    assert!(
        !stdout.contains("(\""),
        "Debug representation of ParseStatus must not leak into list output, stdout={stdout}"
    );
}
