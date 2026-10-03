//! `skillfs classify` must surface skills that fail to load.
//!
//! A skill whose `SKILL.md` cannot be loaded (unreadable, oversized past the
//! 1 MiB `max_skill_size`, ...) is absent from the store, so it silently
//! dropped out of the generated `skillfs-views.toml` with exit code 0 and no
//! indication — while `validate` on the same tree reported the failure. The
//! views config is the mount-time allowlist, so the omission was permanent.

use std::path::Path;
use std::process::Command;

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_skillfs")
}

const VALID_SKILL: &str = "---\nname: good-skill\ndescription: A valid skill\n---\nBody.\n";

fn create_skill_dir(parent: &Path, name: &str, content: &str) {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).expect("create skill dir");
    std::fs::write(dir.join("SKILL.md"), content).expect("write SKILL.md");
}

/// One valid skill plus one whose SKILL.md exceeds the 1 MiB parse limit —
/// the audit's manual repro shape.
fn tree_with_unloadable_skill(parent: &Path) {
    create_skill_dir(parent, "good-skill", VALID_SKILL);
    let big = parent.join("big-skill");
    std::fs::create_dir_all(&big).expect("create big skill dir");
    // 1.1 MB of body: parse_skill_file_with_limit rejects it with
    // "file too large: ... bytes (max 1048576)".
    let oversized = format!("---\nname: big-skill\ndescription: too big\n---\n{}\n", "a".repeat(1_100_000));
    std::fs::write(big.join("SKILL.md"), oversized).expect("write oversized SKILL.md");
}

#[test]
fn classify_warns_on_load_errors_and_omits_them_from_views() {
    let source = tempfile::tempdir().expect("source tempdir");
    tree_with_unloadable_skill(source.path());

    let out = Command::new(bin_path())
        .args(["classify", source.path().to_str().unwrap()])
        .output()
        .expect("invoke skillfs classify");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // Exit-code policy: warnings, not failure — classify still produces a
    // valid config for the skills that did load (cmd_mount's precedent for
    // the same error vector).
    assert!(
        out.status.success(),
        "classify must still exit 0 with warnings, stdout={stdout} stderr={stderr}"
    );

    // The load error is surfaced on stderr: the skill, its path, and the
    // parser's reason.
    assert!(
        stderr.contains("skipped 1 unloadable skill"),
        "stderr must carry the skipped summary, got: {stderr}"
    );
    assert!(
        stderr.contains("big-skill/SKILL.md"),
        "stderr must name the unloadable skill's path, got: {stderr}"
    );
    assert!(
        stderr.contains("file too large"),
        "stderr must carry the LoadError reason, got: {stderr}"
    );

    // The written views config lists the healthy skill and omits the
    // unloadable one (the omission is the documented behavior; the warning
    // above is what makes it visible).
    let views_path = source.path().join("skillfs-views.toml");
    let views = std::fs::read_to_string(&views_path)
        .unwrap_or_else(|_| panic!("views config must be written: {views_path:?}"));
    assert!(views.contains("good-skill"), "views={views}");
    assert!(!views.contains("big-skill"), "views={views}");
}

#[test]
fn classify_on_healthy_tree_is_unchanged() {
    let source = tempfile::tempdir().expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);

    let out = Command::new(bin_path())
        .args(["classify", source.path().to_str().unwrap()])
        .output()
        .expect("invoke skillfs classify");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "classify on a healthy tree must succeed, stdout={stdout} stderr={stderr}"
    );
    assert!(
        !stderr.contains("unloadable"),
        "no load-error warning may appear for a healthy tree, got: {stderr}"
    );
    let views_path = source.path().join("skillfs-views.toml");
    let views = std::fs::read_to_string(&views_path)
        .unwrap_or_else(|_| panic!("views config must be written: {views_path:?}"));
    assert!(views.contains("good-skill"), "views={views}");
}

#[test]
fn validate_reports_the_same_load_error() {
    // Cross-check that `validate` and `classify` now agree the skill failed:
    // validate reports the failure as a failed skill (and non-zero exit).
    let source = tempfile::tempdir().expect("source tempdir");
    tree_with_unloadable_skill(source.path());

    let out = Command::new(bin_path())
        .args(["validate", source.path().to_str().unwrap()])
        .output()
        .expect("invoke skillfs validate");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "validate must fail on an unloadable skill, stdout={stdout}"
    );
    assert!(
        stdout.contains("file too large"),
        "validate must report the same LoadError, stdout={stdout}"
    );
}
