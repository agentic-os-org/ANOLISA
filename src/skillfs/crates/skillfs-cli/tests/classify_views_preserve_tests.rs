//! `skillfs classify` must not replace an existing views config it cannot read.
//!
//! `ViewsConfig::load` returns `None` both for an absent file and for one
//! that fails to parse or read, and `cmd_classify` treated every `None` as
//! "no config yet": it generated a fresh major/other split and saved it over
//! the existing file. A user who mistyped their hand-edited
//! `skillfs-views.toml` and re-ran classify lost the view assignments the
//! file still held, even though the command's own contract is to report an
//! existing config "instead of overwriting" it.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_skillfs")
}

const VALID_SKILL: &str = "---\nname: good-skill\ndescription: A valid skill\n---\nBody.\n";

/// Unclosed array-of-tables header: not valid TOML, so `ViewsConfig::load`
/// logs a parse warning and returns `None`.
const MALFORMED_VIEWS: &str = "[[view]\nname = \"major\"\ndefault = true\n";

/// Valid config whose description marks it as user-authored; the generated
/// config never contains this text.
const CUSTOM_VIEWS: &str = r#"# hand-written
[[view]]
name = "major"
default = true
description = "custom user view"
skills = ["good-skill"]
"#;

fn create_skill_dir(parent: &Path, name: &str, content: &str) {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).expect("create skill dir");
    std::fs::write(dir.join("SKILL.md"), content).expect("write SKILL.md");
}

fn run_classify(source: &Path) -> std::process::Output {
    Command::new(bin_path())
        .args(["classify", source.to_str().unwrap()])
        .output()
        .expect("invoke skillfs classify")
}

#[test]
fn classify_refuses_to_overwrite_an_unparseable_views_config() {
    let source = tempfile::tempdir().expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);
    let views = source.path().join("skillfs-views.toml");
    std::fs::write(&views, MALFORMED_VIEWS).expect("seed malformed views config");

    let out = run_classify(source.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !out.status.success(),
        "classify must fail while the existing views config is unreadable; \
         stdout={stdout} stderr={stderr}"
    );
    assert!(
        !stdout.contains("Written skillfs-views.toml"),
        "classify must not claim it wrote the config: {stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(&views).expect("config still exists"),
        MALFORMED_VIEWS,
        "the existing config must be left byte-identical for the user to repair"
    );
}

#[test]
fn classify_generates_a_views_config_when_none_exists() {
    let source = tempfile::tempdir().expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);

    let out = run_classify(source.path());
    assert!(
        out.status.success(),
        "classify on a config-less tree must keep succeeding; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let views = source.path().join("skillfs-views.toml");
    let written = std::fs::read_to_string(&views).expect("generated config");
    assert!(
        written.contains("[[view]]") && written.contains("good-skill"),
        "generated config must list the skill: {written}"
    );
}

#[test]
fn classify_reports_an_existing_valid_views_config_without_rewriting() {
    let source = tempfile::tempdir().expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);
    let views = source.path().join("skillfs-views.toml");
    std::fs::write(&views, CUSTOM_VIEWS).expect("seed valid views config");

    let out = run_classify(source.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "reporting an existing valid config stays a success; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("already exists"),
        "classify must report the existing config: {stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(&views).expect("config still exists"),
        CUSTOM_VIEWS,
        "a valid existing config must not be rewritten"
    );
}

#[test]
fn classify_concurrent_creation_publishes_exactly_one_config() {
    let source = tempfile::tempdir().expect("source tempdir");
    create_skill_dir(source.path(), "alpha", VALID_SKILL);
    create_skill_dir(source.path(), "beta", VALID_SKILL);

    // Every run starts with the config absent; the create-only publication
    // must let exactly one process write the file. Runs that start later see
    // the published config and report it instead, so "Written" is the
    // discriminator, not the exit status.
    let mut children: Vec<std::process::Child> = (0..16)
        .map(|index| {
            Command::new(bin_path())
                .args([
                    "classify",
                    source.path().to_str().unwrap(),
                    "--primary-count",
                    &(index % 3 + 1).to_string(),
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn skillfs classify")
        })
        .collect();

    let mut written = 0usize;
    for child in &mut children {
        let mut stdout = String::new();
        child
            .stdout
            .take()
            .expect("piped stdout")
            .read_to_string(&mut stdout)
            .expect("read classify stdout");
        let status = child.wait().expect("wait for classify");
        assert!(
            status.success() || !stdout.contains("Written skillfs-views.toml"),
            "a failed classify must not claim it wrote the config: {stdout}"
        );
        if stdout.contains("Written skillfs-views.toml") {
            written += 1;
        }
    }
    assert_eq!(
        written, 1,
        "exactly one concurrent classify may publish the config"
    );

    let views = std::fs::read_to_string(source.path().join("skillfs-views.toml"))
        .expect("published config");
    assert!(
        views.contains("[[view]]"),
        "published config must be complete: {views}"
    );
    let leftovers: Vec<String> = std::fs::read_dir(source.path())
        .expect("read source dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "no staging file may be left behind: {leftovers:?}"
    );
}
