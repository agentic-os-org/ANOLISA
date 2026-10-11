//! CLI side of the legacy-fence shape: a skill whose SKILL.md
//! frontmatter is closed by a `----` thematic break.
//!
//! While that shape parsed degraded, classify's load-error warning
//! never fired for it (it is not a LoadError), so the skill kept
//! flowing into the generated views config with silently empty
//! metadata. After the closer-rule fix the skill classifies with its
//! real metadata and still produces no spurious warnings; an oversized
//! SKILL.md (a real LoadError) still warns — the contrast case.

use std::path::Path;
use std::process::Command;

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_skillfs")
}

fn create_skill_dir(parent: &Path, name: &str, content: &str) {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).expect("create skill dir");
    std::fs::write(dir.join("SKILL.md"), content).expect("write SKILL.md");
}

#[test]
fn classify_serves_legacy_fence_skill_with_real_metadata() {
    let source = tempfile::tempdir().expect("source tempdir");
    create_skill_dir(
        source.path(),
        "fine",
        "---\nname: fine\ndescription: good one\n---\nBody.\n",
    );
    // Frontmatter closed by a Markdown thematic break: silently degraded
    // by the exact-closer rule, restored by the dash-run closer.
    create_skill_dir(
        source.path(),
        "legacy-closer",
        "---\nname: legacy-closer\ndescription: used to parse\n----\nBody.\n",
    );

    let out = Command::new(bin_path())
        .args(["classify", source.path().to_str().unwrap()])
        .output()
        .expect("invoke skillfs classify");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "classify keeps succeeding, stderr={stderr}"
    );
    assert!(
        !stderr.contains("failed to load"),
        "the legacy-fence skill must not produce load warnings"
    );
    assert!(
        !stderr.contains("WARN") && !stdout.contains("degrad") && !stderr.contains("degrad"),
        "no degradation diagnostic anywhere: stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("legacy-closer") || {
            let views = std::fs::read_to_string(source.path().join("skillfs-views.toml"))
                .unwrap_or_default();
            views.contains("legacy-closer")
        },
        "the legacy-fence skill is classified into the views config, stdout={stdout}"
    );
}

#[test]
fn classify_still_warns_for_real_load_errors() {
    let source = tempfile::tempdir().expect("source tempdir");
    create_skill_dir(
        source.path(),
        "fine",
        "---\nname: fine\ndescription: ok\n---\nBody.\n",
    );
    let big = source.path().join("big-skill");
    std::fs::create_dir_all(&big).expect("create big dir");
    std::fs::write(
        big.join("SKILL.md"),
        format!(
            "---\nname: big\ndescription: too big\n---\n{}\n",
            "a".repeat(1_100_000)
        ),
    )
    .expect("write oversized SKILL.md");

    let out = Command::new(bin_path())
        .args(["classify", source.path().to_str().unwrap()])
        .output()
        .expect("invoke skillfs classify");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("big-skill") && stderr.contains("failed to load"),
        "contrast case must warn, stderr={stderr}"
    );
}
