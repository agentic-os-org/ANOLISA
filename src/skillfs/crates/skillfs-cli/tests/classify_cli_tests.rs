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
    let oversized = format!(
        "---\nname: big-skill\ndescription: too big\n---\n{}\n",
        "a".repeat(1_100_000)
    );
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
        stderr.contains("1 skill(s) failed to load and could not be classified"),
        "stderr must carry the outcome-neutral summary, got: {stderr}"
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
        !stderr.contains("failed to load"),
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

/// A tree whose failing skill DIRECTORIES embed a raw newline (line
/// fabrication) and an OSC 777 sequence (a live terminal command): the
/// load-error diagnostics — the warn! fields and the stderr summary alike,
/// both printed in the default configuration — must escape both instead of
/// passing them through.
#[test]
fn classify_warning_escapes_control_characters_in_paths() {
    let source = tempfile::tempdir().expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);
    // Both hostile names get oversized SKILL.md files so they fail with
    // "file too large" — failing skills, whose names reach the warning.
    let oversized = format!(
        "---\nname: hostile\ndescription: d\n---\n{}\n",
        "a".repeat(1_100_000)
    );
    for hostile in [
        "evil\n  Injected: trusted diagnostic line",
        "ansi\u{1b}]777;id\u{7}",
    ] {
        let dir = source.path().join(hostile);
        std::fs::create_dir_all(&dir).expect("create hostile skill dir");
        std::fs::write(dir.join("SKILL.md"), &oversized).expect("write oversized SKILL.md");
    }

    let out = Command::new(bin_path())
        .args(["classify", source.path().to_str().unwrap()])
        .output()
        .expect("invoke skillfs classify");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "classify must still exit 0, stdout={stdout} stderr={stderr}"
    );

    // The attacker-controlled bytes never reach stderr raw: no actual
    // newline after "evil" (the escaped form is `evil\n` with a literal
    // backslash), and no raw ESC following "ansi". The CLI's own ANSI log
    // styling may still contain ESC bytes; what must not is the payload.
    assert!(
        !stderr.contains("evil\n"),
        "a newline inside a skill name must be escaped, not printed raw: {stderr:?}"
    );
    assert!(
        !stderr.contains("ansi\u{1b}"),
        "the OSC sequence inside a skill name must be escaped, not printed raw: {stderr:?}"
    );
    assert!(
        !stderr
            .lines()
            .any(|l| l.trim_start().starts_with("Injected:")),
        "the hostile name must not fabricate new diagnostic lines: {stderr:?}"
    );
    // The hostile names are still reported, in escaped form.
    assert!(
        stderr.contains("evil\\n"),
        "the newline-named skill must be reported with an escaped newline: {stderr:?}"
    );
    assert!(
        stderr.contains("\\x1b]777;id\\x07"),
        "the OSC-named skill must be reported with an escaped ESC: {stderr:?}"
    );
    // The healthy skill's generated views config is unaffected.
    let views = std::fs::read_to_string(source.path().join("skillfs-views.toml"))
        .expect("views config must be written");
    assert!(views.contains("good-skill"), "views={views}");
}

/// When a views config already exists, classify only reports it — the
/// warning must not claim the unloadable skill "will NOT be listed" while
/// the existing file (and this run's own stdout) still lists it.
#[test]
fn classify_warning_stays_accurate_when_views_already_exist() {
    let source = tempfile::tempdir().expect("source tempdir");
    tree_with_unloadable_skill(source.path());
    // A pre-existing views config that already lists the unloadable skill
    // (a stale hand-managed config — the flow the old wording got wrong).
    let views_path = source.path().join("skillfs-views.toml");
    let original_views = "[[view]]\nname = \"major\"\ndefault = true\ndescription = \"hand-managed\"\nskills = [\"good-skill\", \"big-skill\"]\n";
    std::fs::write(&views_path, original_views).expect("seed existing views config");

    let out = Command::new(bin_path())
        .args(["classify", source.path().to_str().unwrap()])
        .output()
        .expect("invoke skillfs classify");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "classify on an existing config must exit 0, stdout={stdout} stderr={stderr}"
    );

    // The existing-config flow reports the file instead of overwriting it,
    // and still lists the unloadable skill — so the warning must make no
    // claim that the skill is absent from skillfs-views.toml.
    assert!(
        stdout.contains("skillfs-views.toml already exists"),
        "existing config must be reported, stdout={stdout}"
    );
    assert!(
        stdout.contains("big-skill"),
        "the existing entry for the unloadable skill is still listed, stdout={stdout}"
    );
    assert!(
        stderr.contains("1 skill(s) failed to load and could not be classified"),
        "the load-error warning must appear, got: {stderr}"
    );
    assert!(
        !stderr.contains("will NOT be listed"),
        "the warning must stay outcome-neutral when a config already exists, got: {stderr}"
    );
    // The existing file is untouched.
    let after = std::fs::read_to_string(&views_path).expect("views config must survive");
    assert_eq!(
        after, original_views,
        "existing config must not be rewritten"
    );
}

/// With `--dry-run` nothing is written at all — the warning must make no
/// claim about a file that is never produced.
#[test]
fn classify_warning_stays_accurate_in_dry_run() {
    let source = tempfile::tempdir().expect("source tempdir");
    tree_with_unloadable_skill(source.path());

    let out = Command::new(bin_path())
        .args(["classify", source.path().to_str().unwrap(), "--dry-run"])
        .output()
        .expect("invoke skillfs classify --dry-run");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "dry-run must exit 0, stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("[dry-run] Would write skillfs-views.toml"),
        "dry-run must preview the config, stdout={stdout}"
    );
    assert!(
        stderr.contains("1 skill(s) failed to load and could not be classified"),
        "the load-error warning must still appear in dry-run, got: {stderr}"
    );
    assert!(
        !stderr.contains("will NOT be listed"),
        "dry-run writes nothing; the warning must stay outcome-neutral, got: {stderr}"
    );
    assert!(
        !views_path(source.path()).exists(),
        "dry-run must not write skillfs-views.toml"
    );
}

/// The dry-run listing prints store skill names — adopted verbatim from
/// directory names — on stdout, where the same line-fabrication and
/// terminal-command forgery the load-error diagnostics escape applies.
#[test]
fn classify_dry_run_listing_escapes_control_characters_in_skill_names() {
    let source = tempfile::tempdir().expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);
    // Both hostile names carry a loadable SKILL.md, so they reach the
    // dry-run listing (the store adopts the directory name verbatim).
    for hostile in ["evil\n  Injected: trusted line", "ansi\u{1b}]777;id\u{7}"] {
        create_skill_dir(source.path(), hostile, VALID_SKILL);
    }

    let out = Command::new(bin_path())
        .args(["classify", source.path().to_str().unwrap(), "--dry-run"])
        .output()
        .expect("invoke skillfs classify --dry-run");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "dry-run must exit 0, stdout={stdout} stderr={stderr}"
    );

    // No attacker-controlled control byte reaches stdout raw...
    assert!(
        !stdout.contains("evil\n"),
        "a newline inside a skill name must be escaped, not printed raw: {stdout:?}"
    );
    assert!(
        !stdout.contains("ansi\u{1b}"),
        "the OSC sequence inside a skill name must be escaped, not printed raw: {stdout:?}"
    );
    assert!(
        !stdout
            .lines()
            .any(|l| l.trim_start().starts_with("Injected:")),
        "the hostile name must not fabricate new listing lines: {stdout:?}"
    );
    // ...and the names are still reported, in escaped form.
    assert!(
        stdout.contains("evil\\n"),
        "the newline-named skill must be listed with an escaped newline: {stdout:?}"
    );
    assert!(
        stdout.contains("\\x1b]777;id\\x07"),
        "the OSC-named skill must be listed with an escaped ESC: {stdout:?}"
    );
    assert!(
        stdout.contains("good-skill"),
        "healthy skill names must stay readable: {stdout:?}"
    );
}

/// The existing-config report echoes view names, descriptions and skill
/// entries straight out of `skillfs-views.toml`; those fields are
/// tree-controlled content and get the same escaping on stdout.
#[test]
fn classify_existing_views_report_escapes_control_characters() {
    let source = tempfile::tempdir().expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);
    // TOML string escapes so the parsed fields carry real control bytes: a
    // newline in a skill entry and an OSC 777 sequence in the view name.
    let hostile_views = "[[view]]\nname = \"major\\u001b]777;id\\u0007\"\ndefault = true\ndescription = \"desc\\u001b[2J\"\nskills = [\"good-skill\", \"evil\\nline\"]\n";
    let views_path = views_path(source.path());
    std::fs::write(&views_path, hostile_views).expect("seed hostile views config");

    let out = Command::new(bin_path())
        .args(["classify", source.path().to_str().unwrap()])
        .output()
        .expect("invoke skillfs classify");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "classify on an existing config must exit 0, stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("skillfs-views.toml already exists"),
        "existing config must be reported, stdout={stdout}"
    );

    assert!(
        !stdout.contains("evil\nline"),
        "a newline inside a views entry must be escaped, not printed raw: {stdout:?}"
    );
    assert!(
        !stdout.contains("desc\u{1b}"),
        "an ESC inside a view description must be escaped, not printed raw: {stdout:?}"
    );
    assert!(
        !stdout.contains("major\u{1b}"),
        "an ESC inside a view name must be escaped, not printed raw: {stdout:?}"
    );
    assert!(
        stdout.contains("evil\\nline"),
        "the newline entry must be reported with an escaped newline: {stdout:?}"
    );
    assert!(
        stdout.contains("major\\x1b]777;id\\x07"),
        "the view name must be reported with an escaped ESC: {stdout:?}"
    );
    assert!(
        stdout.contains("desc\\x1b[2J"),
        "the description must be reported with an escaped ESC: {stdout:?}"
    );
    // The existing file itself is untouched.
    let after = std::fs::read_to_string(&views_path).expect("views config must survive");
    assert_eq!(
        after, hostile_views,
        "existing config must not be rewritten"
    );
}

fn views_path(source: &Path) -> std::path::PathBuf {
    source.join("skillfs-views.toml")
}
