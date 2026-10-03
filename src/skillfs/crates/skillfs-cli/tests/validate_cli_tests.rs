//! CLI validate subcommand tests.
//!
//! Verifies that `skillfs validate` correctly reports parse failures,
//! warnings, and success in both text and JSON output, with correct
//! exit codes.

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

// ─────────────────────────────────────────────────────────────────────────────
// 1. invalid-only text
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn invalid_only_text_exits_nonzero() {
    let source = tempfile::tempdir().expect("source tempdir");
    create_skill_dir(source.path(), "bad-yaml", ERROR_SKILL);
    create_skill_dir(source.path(), "empty-skill", "");

    let out = Command::new(bin_path())
        .args(["validate", source.path().to_str().unwrap()])
        .output()
        .expect("invoke skillfs validate");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "expected non-zero exit code, stdout={stdout}"
    );
    assert!(
        !stdout.contains("All skills loaded successfully"),
        "must not claim all-ok when errors exist, stdout={stdout}"
    );
    assert!(
        stdout.contains("failed") || stdout.contains("✗"),
        "should report failures, stdout={stdout}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. invalid-only JSON
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn invalid_only_json_exits_nonzero() {
    let source = tempfile::tempdir().expect("source tempdir");
    create_skill_dir(source.path(), "bad-yaml", ERROR_SKILL);

    let out = Command::new(bin_path())
        .args([
            "validate",
            source.path().to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .expect("invoke skillfs validate --format json");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "expected non-zero exit code, stdout={stdout}"
    );

    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout should be valid JSON");
    let failed = json["failed"].as_u64().expect("failed field");
    assert!(failed > 0, "failed should be > 0, got {failed}");
    let errors = json["errors"].as_array().expect("errors array");
    assert!(!errors.is_empty(), "errors array should not be empty");
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. mixed JSON (valid + degraded + error)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn mixed_json_counts_correctly() {
    let source = tempfile::tempdir().expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);
    create_skill_dir(source.path(), "degraded-skill", DEGRADED_SKILL);
    create_skill_dir(source.path(), "bad-yaml", ERROR_SKILL);

    let out = Command::new(bin_path())
        .args([
            "validate",
            source.path().to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .expect("invoke skillfs validate --format json");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "expected non-zero exit (has errors), stdout={stdout}"
    );

    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout should be valid JSON");

    let success = json["success"].as_u64().expect("success field");
    assert_eq!(success, 1, "only Ok skill should count as success");

    let degraded = json["degraded"].as_u64().expect("degraded field");
    assert_eq!(degraded, 1, "one degraded skill expected");

    let failed = json["failed"].as_u64().expect("failed field");
    assert_eq!(failed, 1, "one error skill expected as failed");

    let errors = json["errors"].as_array().expect("errors array");
    assert_eq!(errors.len(), 1, "one error entry expected");

    let warnings = json["warnings"].as_array().expect("warnings array");
    assert_eq!(warnings.len(), 1, "one warning entry expected");
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. degraded-only
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn degraded_only_exits_zero() {
    let source = tempfile::tempdir().expect("source tempdir");
    create_skill_dir(source.path(), "degraded-skill", DEGRADED_SKILL);

    let out = Command::new(bin_path())
        .args(["validate", source.path().to_str().unwrap()])
        .output()
        .expect("invoke skillfs validate");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "degraded-only should exit 0, stdout={stdout}"
    );
    assert!(
        !stdout.contains("All skills loaded successfully"),
        "should not claim all-ok when degraded, stdout={stdout}"
    );
    assert!(
        stdout.contains("degraded") || stdout.contains("⚠"),
        "should show degraded info, stdout={stdout}"
    );
}

#[test]
fn degraded_only_json_exits_zero() {
    let source = tempfile::tempdir().expect("source tempdir");
    create_skill_dir(source.path(), "degraded-skill", DEGRADED_SKILL);

    let out = Command::new(bin_path())
        .args([
            "validate",
            source.path().to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .expect("invoke skillfs validate --format json");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "degraded-only should exit 0, stdout={stdout}"
    );

    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout should be valid JSON");

    let degraded = json["degraded"].as_u64().expect("degraded field");
    assert!(degraded > 0, "degraded should be > 0");

    let failed = json["failed"].as_u64().expect("failed field");
    assert_eq!(failed, 0, "no failures expected");

    let warnings = json["warnings"].as_array().expect("warnings array");
    assert!(!warnings.is_empty(), "warnings should not be empty");
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. all-ok
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn all_ok_prints_success_message() {
    let source = tempfile::tempdir().expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);

    let out = Command::new(bin_path())
        .args(["validate", source.path().to_str().unwrap()])
        .output()
        .expect("invoke skillfs validate");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "all-ok should exit 0, stdout={stdout}"
    );
    assert!(
        stdout.contains("All skills loaded successfully"),
        "should print success message, stdout={stdout}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. skills[].status values in JSON output
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn json_skill_status_fields_are_clean_names() {
    let source = tempfile::tempdir().expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);
    create_skill_dir(source.path(), "degraded-skill", DEGRADED_SKILL);
    create_skill_dir(source.path(), "bad-yaml", ERROR_SKILL);

    let out = Command::new(bin_path())
        .args([
            "validate",
            source.path().to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .expect("invoke skillfs validate --format json");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout should be valid JSON");

    let skills = json["skills"].as_array().expect("skills array");
    assert!(skills.len() >= 3, "expected 3 skills, got {skills:?}");

    let statuses: std::collections::BTreeMap<String, String> = skills
        .iter()
        .map(|s| {
            (
                s["name"].as_str().expect("skill name").to_string(),
                s["status"].as_str().expect("status string").to_string(),
            )
        })
        .collect();

    // The status field must carry the bare status name — "ok", "degraded",
    // or "error" — so JSON consumers can match on it. Debug-repr leakage
    // like `degraded("missing description")` embeds the message (already
    // reported separately) into the status string.
    for (name, status) in &statuses {
        assert!(
            matches!(status.as_str(), "ok" | "degraded" | "error"),
            "skill {name} has non-canonical status {status:?}"
        );
    }
    assert_eq!(statuses["good-skill"], "ok", "good-skill status");
    assert_eq!(statuses["degraded-skill"], "degraded", "degraded status");
    assert_eq!(statuses["bad-yaml"], "error", "bad-yaml status");
}
