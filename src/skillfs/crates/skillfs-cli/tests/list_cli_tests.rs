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

/// A SKILL.md past the 1 MiB parse limit cannot be loaded at all: the skill
/// is absent from the store, so list must report that it skipped it instead
/// of silently omitting it (an all-failed tree otherwise looks empty).
fn create_unloadable_skill(parent: &Path, name: &str) {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).expect("create skill dir");
    let oversized = format!(
        "---\nname: {name}\ndescription: too big\n---\n{}\n",
        "a".repeat(1_100_000)
    );
    std::fs::write(dir.join("SKILL.md"), oversized).expect("write oversized SKILL.md");
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

#[test]
fn list_reports_skills_that_fail_to_load() {
    let source = tempfile::tempdir().expect("source tempdir");
    create_unloadable_skill(source.path(), "big-skill");

    let out = Command::new(bin_path())
        .args(["list", source.path().to_str().unwrap()])
        .output()
        .expect("invoke skillfs list");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "list stays a non-fatal inspection for the tree, stderr={stderr}"
    );
    assert!(
        stderr.contains("big-skill"),
        "list must name the skill it could not load, stderr={stderr}"
    );
    assert!(
        stderr.contains("unloadable"),
        "list must say the skill was skipped instead of silently omitting it, \
         stderr={stderr}"
    );
}

#[test]
fn list_keeps_listing_loaded_skills_when_another_fails() {
    let source = tempfile::tempdir().expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);
    create_unloadable_skill(source.path(), "big-skill");

    let out = Command::new(bin_path())
        .args(["list", source.path().to_str().unwrap()])
        .output()
        .expect("invoke skillfs list");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "list should still succeed, stderr={stderr}"
    );
    assert!(
        stdout.contains("good-skill"),
        "skills that loaded must keep listing, stdout={stdout}"
    );
    assert!(
        stderr.contains("big-skill") && stderr.contains("unloadable"),
        "the skipped skill must be reported, stderr={stderr}"
    );
}

#[test]
fn list_escapes_terminal_control_bytes_in_load_error_diagnostics() {
    let source = tempfile::tempdir().expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);
    // A directory name carrying ESC[2J and a newline: raw diagnostics would
    // let an unloadable skill clear the terminal and forge a second line.
    let evil_name = "evil\u{1b}[2J-name\nline2";
    create_unloadable_skill(source.path(), evil_name);

    let out = Command::new(bin_path())
        .args(["list", source.path().to_str().unwrap()])
        .output()
        .expect("invoke skillfs list");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "list stays non-fatal, stderr={stderr}"
    );
    assert!(
        stdout.contains("good-skill"),
        "loadable skills keep listing, stdout={stdout}"
    );
    assert!(
        !stderr.contains("evil\u{1b}"),
        "the raw ESC byte must not reach stderr after the name, stderr={stderr}"
    );
    assert!(
        stderr.contains("evil\\u{1b}[2J-name\\nline2"),
        "the escaped name must be reported, stderr={stderr}"
    );
}

/// A SKILL.md whose frontmatter explicitly disables the skill.
const DISABLED_SKILL: &str = "---\nname: disabled-skill\ndescription: A disabled skill\nenabled: false\n---\n# Disabled Skill\n";

/// `--enabled-only` on a tree where every skill is disabled must not print
/// the `Skills in <source>:` section header followed by zero entries: the
/// header promises content the filtered view does not have, and a script
/// parsing the output sees a dangling section instead of the explicit
/// empty-result summary the empty-store case prints.
#[test]
fn list_enabled_only_reports_an_all_disabled_tree() {
    let source = tempfile::tempdir().expect("source tempdir");
    create_skill_dir(source.path(), "disabled-skill", DISABLED_SKILL);

    let out = Command::new(bin_path())
        .args(["list", "--enabled-only", source.path().to_str().unwrap()])
        .output()
        .expect("invoke skillfs list --enabled-only");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "list --enabled-only should succeed, stdout={stdout}"
    );
    assert!(
        stdout.contains("No enabled skills found in"),
        "an all-disabled tree must print the empty-result summary, stdout={stdout}"
    );
    assert!(
        !stdout.contains("Skills in "),
        "the section header must not promise entries the filtered view lacks, stdout={stdout}"
    );
}

/// The mixed tree pins the filter itself: `--enabled-only` lists the enabled
/// skill and omits the disabled one, so the all-disabled summary above cannot
/// regress into an unconditional "no enabled skills" answer.
#[test]
fn list_enabled_only_keeps_enabled_skills_and_drops_disabled_ones() {
    let source = tempfile::tempdir().expect("source tempdir");
    create_skill_dir(source.path(), "disabled-skill", DISABLED_SKILL);
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);

    let out = Command::new(bin_path())
        .args(["list", "--enabled-only", source.path().to_str().unwrap()])
        .output()
        .expect("invoke skillfs list --enabled-only");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "list --enabled-only should succeed, stdout={stdout}"
    );
    assert!(
        stdout.contains("good-skill"),
        "the enabled skill must stay listed, stdout={stdout}"
    );
    assert!(
        !stdout.contains("disabled-skill"),
        "the disabled skill must be omitted from the --enabled-only listing, stdout={stdout}"
    );
}
