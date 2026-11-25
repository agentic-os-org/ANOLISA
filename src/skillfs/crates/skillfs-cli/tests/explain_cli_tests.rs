//! Configuration and managed-worker explain reports exercised through the real CLI.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};

#[path = "explain_cli_tests/covered_mount.rs"]
mod covered_mount;

const VALID: &str =
    "---\nname: metadata-name\ndescription: A valid skill\n---\nPrivate skill body.\n";
const BROKEN: &str = "---\nname: [invalid yaml\n  broken: {{{}\n---\nPrivate broken body.\n";
const DEGRADED: &str = "---\nname: metadata-name\n---\n";

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_skillfs"));
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.join("home"))
        .env("XDG_RUNTIME_DIR", root.join("runtime"))
        .env("SKILLFS_SLS_OPS_PATH", root.join("ops.jsonl"))
        .current_dir(root);
    command
}

fn skill(source: &Path, name: &str, content: &str) -> PathBuf {
    let dir = source.join(name);
    fs::create_dir_all(&dir).expect("create skill directory");
    let path = dir.join("SKILL.md");
    fs::write(&path, content).expect("write skill");
    path
}

fn config(root: &Path, sources: &[PathBuf]) -> PathBuf {
    config_at(root, &root.join("new-mount"), sources)
}

fn config_at(root: &Path, mount: &Path, sources: &[PathBuf]) -> PathBuf {
    let path = root.join("mount.toml");
    let sources: Vec<_> = sources
        .iter()
        .map(|source| source.to_str().expect("UTF-8 fixture path"))
        .collect();
    let value = toml::Value::try_from(json!({
        "mountpoint": mount,
        "sources": sources,
    }))
    .expect("mount config value");
    fs::write(&path, toml::to_string(&value).expect("serialize config"))
        .expect("write mount config");
    path
}

fn invoke(root: &Path, config: &Path, args: &[&str]) -> Output {
    command(root)
        .arg("explain")
        .arg("--config")
        .arg(config)
        .args(args)
        .output()
        .expect("invoke skillfs explain")
}

fn report(root: &Path, config: &Path, args: &[&str], exit: i32) -> Value {
    let mut args = args.to_vec();
    args.extend(["--format", "json"]);
    let output = invoke(root, config, &args);
    assert_eq!(
        output.status.code(),
        Some(exit),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout)
        .expect("stdout must contain exactly one JSON report, without logs");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["target"], "configuration");
    assert_eq!(report["config_path"], json!(config));
    assert_eq!(
        report["status"],
        if exit == 0 { "complete" } else { "incomplete" }
    );
    if exit == 1 {
        assert!(report["blocker"].as_str().is_some_and(|s| !s.is_empty()));
    }
    report
}

fn named<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["skills"]
        .as_array()
        .expect("skills array")
        .iter()
        .find(|skill| skill["name"] == name)
        .expect("reported skill")
}

fn unknown_membership(skill: &Value) {
    assert_eq!(skill["effective_default"], "unknown");
    assert_eq!(skill["default_reason"], "unknown");
    assert!(skill["configured_views"].is_null());
    assert!(skill["secondary_views"].is_null());
}

#[test]
fn complete_schema_preserves_precedence_aliases_categories_and_parse_evidence() {
    let temp = tempfile::tempdir_in("/tmp").expect("fixture");
    let root = temp.path();
    let high = root.join("high");
    let low = root.join("low");
    let last = root.join("last");
    let alias = root.join("alias");
    let winner = skill(&high, "demo", VALID);
    skill(&high, "category/categorized", VALID);
    skill(&high, "z-degraded", DEGRADED);
    skill(
        &high,
        "a-disabled",
        "---\ndescription: Disabled metadata\nenabled: false\n---\n",
    );
    let broken = skill(&low, "demo", BROKEN);
    let lower = skill(&last, "demo", VALID);
    symlink(&high, &alias).expect("source alias");
    let config = config(root, &[high.clone(), alias, low.clone(), last.clone()]);
    let result = report(root, &config, &[], 0);
    assert_eq!(result["mountpoint"], json!(root.join("new-mount")));
    assert_eq!(result["sources"], json!([high, low, last]));
    assert!(result["requested_skill"].is_null());
    assert_eq!(result["selection_status"], "all");
    assert_eq!(
        result["views"],
        json!({
            "path": high.join("skillfs-views.toml"),
            "status": "absent",
            "effective_default": null,
        })
    );
    let skills = result["skills"].as_array().expect("skills");
    assert_eq!(
        skills
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["a-disabled", "categorized", "demo", "z-degraded"]
    );
    let demo = named(&result, "demo");
    assert_eq!(
        demo["selected"],
        json!({
            "source": high,
            "path": winner,
            "parse_status": "ok",
            "parse_message": null,
            "enabled": true,
        })
    );
    assert_eq!(
        fs::read_to_string(demo["selected"]["path"].as_str().unwrap()).unwrap(),
        VALID
    );
    assert_eq!(demo["shadowed"].as_array().unwrap().len(), 2);
    assert_eq!(demo["shadowed"][0]["source"], json!(low));
    assert_eq!(demo["shadowed"][0]["path"], json!(broken));
    assert_eq!(demo["shadowed"][0]["parse_status"], "error");
    assert!(
        demo["shadowed"][0]["parse_message"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    assert_eq!(
        demo["shadowed"][1],
        json!({
            "source": last,
            "path": lower,
            "parse_status": "ok",
            "parse_message": null,
            "enabled": true,
        })
    );
    assert_eq!(
        named(&result, "categorized")["selected"]["path"],
        json!(high.join("category/categorized/SKILL.md"))
    );
    assert_eq!(
        named(&result, "z-degraded")["selected"]["parse_status"],
        "degraded"
    );
    assert!(named(&result, "z-degraded")["selected"]["parse_message"].is_string());
    assert_eq!(named(&result, "a-disabled")["selected"]["enabled"], false);
    for entry in skills {
        assert_eq!(entry["effective_default"], true);
        assert_eq!(entry["default_reason"], "no_views_configuration");
        assert_eq!(entry["configured_views"], json!([]));
        assert_eq!(entry["secondary_views"], json!([]));
    }
    let first = invoke(root, &config, &["--format", "json"]);
    let second = invoke(root, &config, &["--format", "json"]);
    assert!(first.status.success() && second.status.success());
    assert_eq!(
        first.stdout, second.stdout,
        "complete reports must be byte-stable"
    );
    assert!(!String::from_utf8_lossy(&first.stdout).contains("Private skill body"));
}

#[test]
fn views_allow_simultaneous_memberships_and_only_first_default_is_effective() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let root = temp.path();
    let high = root.join("high");
    let low = root.join("low");
    for name in ["both", "secondary", "unassigned", "disabled"] {
        skill(&high, name, VALID);
    }
    skill(
        &high,
        "disabled",
        "---\ndescription: Disabled\nenabled: false\n---\n",
    );
    skill(&low, "from-low", VALID);
    fs::write(
        high.join("skillfs-views.toml"),
        r#"
[[view]]
name = "primary"
default = true
skills = ["both", "disabled", "nonexistent"]
[[view]]
name = "later-default"
default = true
skills = ["both", "secondary"]
[[view]]
name = "another"
skills = ["both", "secondary"]
"#,
    )
    .unwrap();
    fs::write(low.join("skillfs-views.toml"), "this is invalid TOML [").unwrap();
    let config = config(root, &[high.clone(), low]);
    let result = report(root, &config, &[], 0);
    assert_eq!(
        result["views"],
        json!({
            "path": high.join("skillfs-views.toml"),
            "status": "loaded",
            "effective_default": "primary",
        })
    );
    let both = named(&result, "both");
    assert_eq!(both["effective_default"], true);
    assert_eq!(both["default_reason"], "explicit");
    assert_eq!(
        both["configured_views"],
        json!(["primary", "later-default", "another"])
    );
    assert_eq!(both["secondary_views"], json!(["later-default", "another"]));
    let secondary = named(&result, "secondary");
    assert_eq!(secondary["effective_default"], false);
    assert_eq!(secondary["default_reason"], "assigned_secondary");
    assert_eq!(
        secondary["configured_views"],
        json!(["later-default", "another"])
    );
    assert_eq!(
        secondary["secondary_views"],
        json!(["later-default", "another"])
    );
    for name in ["unassigned", "from-low"] {
        let entry = named(&result, name);
        assert_eq!(entry["effective_default"], true);
        assert_eq!(entry["default_reason"], "unassigned");
        assert_eq!(entry["configured_views"], json!([]));
        assert_eq!(entry["secondary_views"], json!([]));
    }
    let disabled = named(&result, "disabled");
    assert_eq!(disabled["selected"]["enabled"], false);
    assert_eq!(disabled["effective_default"], true);
    assert_eq!(disabled["default_reason"], "explicit");
}

#[test]
fn no_default_view_keeps_unassigned_visible_and_secondary_assignments_hidden() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let source = temp.path().join("source");
    skill(&source, "assigned", VALID);
    skill(&source, "unassigned", VALID);
    fs::write(
        source.join("skillfs-views.toml"),
        "[[view]]\nname = 'secondary'\nskills = ['assigned']\n",
    )
    .unwrap();
    let config = config(temp.path(), &[source]);
    let result = report(temp.path(), &config, &[], 0);
    assert_eq!(result["views"]["status"], "loaded");
    assert!(result["views"]["effective_default"].is_null());
    let assigned = named(&result, "assigned");
    assert_eq!(assigned["effective_default"], false);
    assert_eq!(assigned["default_reason"], "assigned_secondary");
    assert_eq!(assigned["configured_views"], json!(["secondary"]));
    assert_eq!(assigned["secondary_views"], json!(["secondary"]));
    assert_eq!(named(&result, "unassigned")["effective_default"], true);
    assert_eq!(named(&result, "unassigned")["default_reason"], "unassigned");
    fs::write(temp.path().join("source/skillfs-views.toml"), "view = []\n").unwrap();
    let empty = report(temp.path(), &config, &[], 0);
    assert_eq!(empty["views"]["status"], "loaded");
    for name in ["assigned", "unassigned"] {
        assert_eq!(named(&empty, name)["effective_default"], true);
        assert_eq!(named(&empty, name)["default_reason"], "unassigned");
        assert_eq!(named(&empty, name)["configured_views"], json!([]));
        assert_eq!(named(&empty, name)["secondary_views"], json!([]));
    }
}

#[test]
fn absent_first_views_never_falls_back_to_later_source() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let high = temp.path().join("high");
    let low = temp.path().join("low");
    fs::create_dir(&high).unwrap();
    skill(&low, "demo", VALID);
    fs::write(
        low.join("skillfs-views.toml"),
        "[[view]]\nname = 'secondary'\nskills = ['demo']\n",
    )
    .unwrap();
    let config = config(temp.path(), &[high.clone(), low]);
    let result = report(temp.path(), &config, &[], 0);
    assert_eq!(
        result["views"]["path"],
        json!(high.join("skillfs-views.toml"))
    );
    assert_eq!(result["views"]["status"], "absent");
    assert_eq!(named(&result, "demo")["effective_default"], true);
    assert_eq!(
        named(&result, "demo")["default_reason"],
        "no_views_configuration"
    );
}

#[test]
fn exact_directory_filter_and_completed_scan_distinguish_not_discovered() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let source = temp.path().join("source");
    skill(&source, "demo", VALID);
    skill(&source, "demo-extra", VALID);
    skill(&source, ".hidden", VALID);
    skill(&source, "too/deep/ignored", VALID);
    symlink(source.join("demo"), source.join("linked")).unwrap();
    let config = config(temp.path(), &[source]);
    let result = report(temp.path(), &config, &["--skill", "demo"], 0);
    assert_eq!(result["requested_skill"], "demo");
    assert_eq!(result["selection_status"], "discovered");
    assert_eq!(result["skills"].as_array().unwrap().len(), 1);
    assert_eq!(result["skills"][0]["name"], "demo");
    for name in [
        "dem",
        "Demo",
        "metadata-name",
        ".hidden",
        "ignored",
        "linked",
        "missing",
    ] {
        let result = report(temp.path(), &config, &["--skill", name], 0);
        assert_eq!(result["requested_skill"], name);
        assert_eq!(result["selection_status"], "not_discovered");
        assert_eq!(result["skills"], json!([]));
    }
}

#[test]
fn config_errors_emit_one_incomplete_report_before_source_validation() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let config = temp.path().join("mount.toml");
    for contents in [
        None,
        Some("sources = ["),
        Some("unrelated = true"),
        Some("mountpoint = '/tmp/mount'\nsources = 'not-an-array'\n"),
    ] {
        if let Some(contents) = contents {
            fs::write(&config, contents).unwrap();
        }
        let result = report(temp.path(), &config, &["--skill", "demo"], 1);
        assert_eq!(result["phase"], "config");
        assert_eq!(result["selection_status"], "unknown");
        assert_eq!(result["sources"], json!([]));
        assert_eq!(result["skills"], json!([]));
        assert_eq!(result["views"]["status"], "unknown");
        assert!(result["mountpoint"].is_null());
    }
}

#[test]
fn later_invalid_source_blocks_all_scanning_even_when_filtered_target_exists() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let source = temp.path().join("first");
    skill(&source, "demo", VALID);
    skill(&source, "broken", BROKEN);
    fs::write(source.join("skillfs-views.toml"), "invalid [").unwrap();
    let missing = temp.path().join("missing");
    let config = config(temp.path(), &[source.clone(), missing.clone()]);
    let result = report(temp.path(), &config, &["--skill", "demo"], 1);
    assert_eq!(result["phase"], "source_validation");
    assert!(
        result["blocker"]
            .as_str()
            .unwrap()
            .contains(missing.to_str().unwrap())
    );
    assert_eq!(result["selection_status"], "unknown");
    assert_eq!(result["skills"], json!([]));
    assert_eq!(result["views"]["status"], "unknown");
    assert_eq!(result["sources"], json!([source]));
}

#[test]
fn invalid_source_shapes_are_validation_errors_without_mount_resources() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let root = temp.path();
    let file = root.join("not-a-directory");
    fs::write(&file, VALID).unwrap();
    for sources in [
        vec![],
        vec![PathBuf::from("relative")],
        vec![file],
        vec![root.to_path_buf()],
    ] {
        let config = config(root, &sources);
        let result = report(root, &config, &[], 1);
        assert_eq!(result["phase"], "source_validation");
        assert_eq!(result["skills"], json!([]));
        assert_eq!(result["views"]["status"], "unknown");
        assert!(!root.join("new-mount").exists());
    }
}

#[test]
fn later_loading_blocker_survives_filter_and_leaves_membership_unknown() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let high = temp.path().join("high");
    let low = temp.path().join("low");
    skill(&high, "demo", VALID);
    let broken = skill(&low, "unrelated", BROKEN);
    fs::write(
        high.join("skillfs-views.toml"),
        "[[view]]\nname='primary'\ndefault=true\nskills=['demo']\n",
    )
    .unwrap();
    let config = config(temp.path(), &[high.clone(), low.clone()]);
    let result = report(temp.path(), &config, &["--skill", "demo"], 1);
    assert_eq!(result["phase"], "source_loading");
    assert_eq!(result["sources"], json!([high, low]));
    assert_eq!(result["selection_status"], "discovered");
    assert_eq!(result["skills"].as_array().unwrap().len(), 1);
    assert!(
        result["blocker"]
            .as_str()
            .unwrap()
            .contains(broken.to_str().unwrap())
    );
    assert_eq!(result["views"]["status"], "unknown");
    unknown_membership(named(&result, "demo"));
    let missing = report(temp.path(), &config, &["--skill", "missing"], 1);
    assert_eq!(missing["phase"], "source_loading");
    assert_eq!(missing["selection_status"], "unknown");
    assert_eq!(missing["skills"], json!([]));
    let failed_target = report(temp.path(), &config, &["--skill", "unrelated"], 1);
    assert_eq!(failed_target["phase"], "source_loading");
    assert_eq!(failed_target["selection_status"], "discovered");
    assert_eq!(
        failed_target["skills"],
        json!([]),
        "a discovered parse failure is not a selected winner"
    );
}

#[test]
fn malformed_unreadable_and_dangling_views_preserve_source_evidence() {
    for kind in ["malformed", "directory", "dangling"] {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let source = temp.path().join("source");
        let selected = skill(&source, "demo", VALID);
        let views = source.join("skillfs-views.toml");
        match kind {
            "malformed" => fs::write(&views, "[[view]\nname = [").unwrap(),
            "directory" => fs::create_dir(&views).unwrap(),
            "dangling" => symlink(source.join("missing-views"), &views).unwrap(),
            _ => unreachable!("fixture cases are fixed"),
        }
        let config = config(temp.path(), &[source.clone()]);
        let result = report(temp.path(), &config, &["--skill", "demo"], 1);
        assert_eq!(result["phase"], "views_loading", "{kind}");
        assert_eq!(
            result["views"],
            json!({
                "path": views,
                "status": "invalid_or_unreadable",
                "effective_default": null,
            })
        );
        assert_eq!(result["sources"], json!([source]));
        assert_eq!(result["selection_status"], "discovered");
        assert_eq!(named(&result, "demo")["selected"]["path"], json!(selected));
        unknown_membership(named(&result, "demo"));
        let missing = report(temp.path(), &config, &["--skill", "missing"], 1);
        assert_eq!(missing["phase"], "views_loading");
        assert_eq!(
            missing["selection_status"], "not_discovered",
            "skill scan completed before views failed"
        );
        assert_eq!(missing["skills"], json!([]));
    }
}

#[test]
fn permission_denied_views_are_not_absent() {
    use std::os::unix::process::CommandExt;

    let temp = tempfile::tempdir_in("/tmp").unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let source = temp.path().join("source");
    skill(&source, "demo", VALID);
    let views = source.join("skillfs-views.toml");
    fs::write(&views, "view = []\n").unwrap();
    fs::set_permissions(&views, fs::Permissions::from_mode(0o000)).unwrap();
    let config = config(temp.path(), &[source]);
    let mut command = command(temp.path());
    // Root bypasses file permissions; exercise the same denial as an ordinary user.
    if unsafe { libc::geteuid() } == 0 {
        command.uid(65534).gid(65534);
    }
    let output = command
        .arg("explain")
        .arg("--config")
        .arg(config)
        .args(["--format", "json"])
        .output()
        .unwrap();
    fs::set_permissions(&views, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["phase"], "views_loading");
    assert_eq!(result["views"]["status"], "invalid_or_unreadable");
    unknown_membership(named(&result, "demo"));
}

#[test]
fn oversized_shadowed_copy_is_nonblocking_but_unselected_oversized_skill_blocks() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let high = temp.path().join("high");
    let low = temp.path().join("low");
    skill(&high, "demo", VALID);
    let oversized = skill(&low, "demo", VALID);
    fs::OpenOptions::new()
        .write(true)
        .open(&oversized)
        .unwrap()
        .set_len(skillfs_core::ParseConfig::default().max_skill_size as u64 + 1)
        .unwrap();
    let config = config(temp.path(), &[high, low.clone()]);
    let result = report(temp.path(), &config, &[], 0);
    let shadow = &named(&result, "demo")["shadowed"][0];
    assert_eq!(shadow["path"], json!(oversized));
    assert_eq!(shadow["source"], json!(low));
    assert_eq!(shadow["parse_status"], "unloadable");
    assert!(shadow["parse_message"].is_string());
    assert!(shadow["enabled"].is_null());
    fs::rename(low.join("demo"), low.join("unrelated")).unwrap();
    let result = report(temp.path(), &config, &["--skill", "demo"], 1);
    assert_eq!(result["phase"], "source_loading");
    unknown_membership(named(&result, "demo"));
}

#[test]
fn combined_source_skill_limit_is_not_bypassed_by_filtering() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let high = temp.path().join("high");
    let low = temp.path().join("low");
    for index in 0..skillfs_core::ParseConfig::default().max_skills {
        skill(&high, &format!("skill-{index:04}"), VALID);
    }
    skill(&low, "extra", VALID);
    let config = config(temp.path(), &[high, low]);
    let result = report(temp.path(), &config, &["--skill", "skill-0000"], 1);
    assert_eq!(result["phase"], "source_loading");
    assert_eq!(result["selection_status"], "discovered");
    unknown_membership(named(&result, "skill-0000"));
    let rejected = report(temp.path(), &config, &["--skill", "extra"], 1);
    assert_eq!(rejected["phase"], "source_loading");
    assert_eq!(rejected["selection_status"], "discovered");
    assert_eq!(rejected["skills"], json!([]));
}

#[test]
fn per_source_limit_blocks_even_when_requested_skill_was_not_encountered() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let source = temp.path().join("source");
    for index in 0..=skillfs_core::ParseConfig::default().max_skills {
        skill(&source, &format!("skill-{index:04}"), VALID);
    }
    let config = config(temp.path(), &[source]);
    let result = report(temp.path(), &config, &["--skill", "missing"], 1);
    assert_eq!(result["phase"], "source_loading");
    assert_eq!(result["selection_status"], "unknown");
    assert_eq!(result["skills"], json!([]));
    assert_eq!(result["views"]["status"], "unknown");
}

fn tree_bytes(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut result = BTreeMap::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            result.insert(path.clone(), None);
            result.extend(tree_bytes(&path));
        } else {
            result.insert(path.clone(), Some(fs::read(path).unwrap()));
        }
    }
    result
}

#[test]
fn complete_and_incomplete_diagnoses_leave_sources_and_managed_state_unchanged() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let root = temp.path();
    let source = root.join("source");
    skill(&source, "demo", VALID);
    fs::create_dir_all(root.join("runtime/skillfs")).unwrap();
    fs::create_dir(root.join("home")).unwrap();
    fs::write(
        root.join("runtime/skillfs/frozen-config.toml"),
        "unread managed snapshot",
    )
    .unwrap();
    fs::write(
        root.join("runtime/skillfs/state.json"),
        "existing managed state",
    )
    .unwrap();
    fs::write(root.join("ops.jsonl"), "existing telemetry\n").unwrap();
    let config = config(root, &[source.clone()]);
    for invalid in [false, true] {
        if invalid {
            fs::write(source.join("skillfs-views.toml"), "invalid [").unwrap();
        }
        let before = tree_bytes(root);
        report(root, &config, &[], i32::from(invalid));
        assert_eq!(
            tree_bytes(root),
            before,
            "diagnosis must not write files or create resources"
        );
        assert!(!root.join("new-mount").exists());
    }
    fs::remove_file(root.join("ops.jsonl")).unwrap();
    report(root, &config, &[], 1);
    assert!(
        !root.join("ops.jsonl").exists(),
        "explain must not start SLS telemetry"
    );
}

#[test]
fn text_reports_configuration_facts_and_escapes_all_tree_controlled_fields() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let root = temp.path();
    let high = root.join("source\nInjected: source\u{1b}]777;id\u{7}");
    let low = root.join("shadow\rsource");
    let name = "evil\nInjected: skill\u{1b}]777;id\u{7}";
    skill(&high, name, VALID);
    skill(&low, name, VALID);
    let view = "view\nInjected: view\u{1b}]777;id\u{7}";
    let views =
        toml::Value::try_from(json!({"view": [{"name": view, "default": true, "skills": [name]}]}))
            .unwrap();
    fs::write(
        high.join("skillfs-views.toml"),
        toml::to_string(&views).unwrap(),
    )
    .unwrap();
    let plain_config = config(root, &[high.clone(), low]);
    let config = root.join("config\nInjected: config.toml");
    fs::rename(plain_config, &config).unwrap();
    let output = invoke(root, &config, &["--skill", name]);
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        !text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    );
    assert!(
        !text
            .lines()
            .any(|line| line.trim_start().starts_with("Injected:"))
    );
    assert!(text.contains("evil\\nInjected: skill\\u{1b}]777;id\\u{7}"));
    assert!(text.contains("source\\nInjected: source\\u{1b}]777;id\\u{7}"));
    assert!(text.contains("view\\nInjected: view\\u{1b}]777;id\\u{7}"));
    assert!(text.contains("config\\nInjected: config.toml"));
    assert!(text.contains("shadow\\rsource"));
    assert!(text.contains("configuration"));
    let result = report(root, &config, &["--skill", name], 0);
    assert_eq!(result["requested_skill"], name);
    assert_eq!(named(&result, name)["configured_views"], json!([view]));
    assert!(
        named(&result, name)["selected"]["parse_message"]
            .as_str()
            .unwrap()
            .contains(name)
    );
    fs::write(high.join("skillfs-views.toml"), "invalid [").unwrap();
    let failure = invoke(
        root,
        &config,
        &["--skill", "missing\nInjected: request\u{1b}"],
    );
    assert_eq!(failure.status.code(), Some(1));
    let text = String::from_utf8(failure.stdout).unwrap();
    assert!(
        !text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    );
    assert!(
        !text
            .lines()
            .any(|line| line.trim_start().starts_with("Injected:"))
    );
    assert!(text.contains("missing\\nInjected: request\\u{1b}"));
}

#[test]
fn usage_errors_require_one_target_and_an_exact_name_for_runtime() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    for args in [
        vec!["explain"],
        vec!["explain", "--mountpoint", "/not-a-runtime-target"],
        vec![
            "explain",
            "--config",
            "mount.toml",
            "--mountpoint",
            "/not-a-runtime-target",
            "--skill",
            "demo",
        ],
        vec!["explain", "--config", "mount.toml", "--format", "yaml"],
    ] {
        let output = command(temp.path()).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
    assert!(!temp.path().join("ops.jsonl").exists());
}

fn runtime_report(root: &Path, name: &str, code: i32) -> Value {
    runtime_report_at(root, &root.join("new-mount"), name, code)
}

fn runtime_report_at(root: &Path, mount: &Path, name: &str, code: i32) -> Value {
    let output = command(root)
        .args(["explain", "--mountpoint"])
        .arg(mount)
        .args(["--skill", name, "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["schema_version"], 1);
    assert_eq!(result["target"], "runtime");
    assert_eq!(result["requested_skill"], name);
    result
}

#[test]
fn unavailable_runtime_query_leaves_an_unmounted_workspace_untouched() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let output = runtime_report(root, "missing", 1);
    assert_eq!(output["status"], "unavailable");
    assert!(output.get("selected").is_none());
    assert!(output.get("effective_default").is_none());
    assert!(fs::read_dir(root).unwrap().next().is_none());
}

fn live_fuse_available() -> bool {
    let available = Path::new("/dev/fuse").exists()
        && Command::new("fusermount3")
            .arg("--version")
            .output()
            .is_ok();
    if !available {
        eprintln!("SKIP: real FUSE is required for live explain");
    }
    available
}

struct StopMount<'a>(&'a Path, &'a Path);

impl Drop for StopMount<'_> {
    fn drop(&mut self) {
        let _ = command(self.0).arg("stop").arg(self.1).output();
    }
}

fn start_mount(root: &Path, config: &Path) {
    let output = command(root)
        .args(["mount", "--managed", "--config"])
        .arg(config)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn paused_worker_reports_the_same_snapshot_for_mount_root_aliases() {
    use std::thread;
    use std::time::{Duration, Instant};

    if !live_fuse_available() {
        return;
    }
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let root = temp.path();
    let sources = [root.join("A"), root.join("B")];
    for source in &sources {
        skill(source, "demo", VALID);
    }
    let config = config(root, &sources);
    let mount = root.join("new-mount");
    let _stop = StopMount(root, &mount);
    start_mount(root, &config);
    let initial = runtime_report(root, "demo", 0);
    assert_eq!(initial["selected"]["source"], json!(sources[0]));
    symlink("new-mount/", root.join("alias")).unwrap();
    fs::create_dir(root.join("other")).unwrap();
    fs::create_dir(root.join("sibling")).unwrap();
    symlink("../sibling", root.join("other/peer")).unwrap();

    struct ResumeWorker(libc::pid_t);
    impl Drop for ResumeWorker {
        fn drop(&mut self) {
            // SAFETY: the positive pid is the worker of this temporary mount.
            unsafe { libc::kill(self.0, libc::SIGCONT) };
        }
    }
    let pid = i32::try_from(initial["worker"]["pid"].as_u64().unwrap()).unwrap();
    assert!(pid > 1);
    // SAFETY: signal only the worker identified by the live report above.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
    let resume = ResumeWorker(pid);
    let timeout = Duration::from_secs(5);
    let status_file = format!("/proc/{pid}/status");
    let paused_at = Instant::now();
    while !fs::read_to_string(&status_file)
        .unwrap()
        .lines()
        .any(|line| line.starts_with("State:") && line.split_whitespace().nth(1) == Some("T"))
    {
        assert!(paused_at.elapsed() < timeout, "worker did not stop");
        thread::sleep(Duration::from_millis(10));
    }
    for path in [
        "new-mount",
        "new-mount/",
        "new-mount/.",
        "alias",
        "alias/",
        "other/peer/../new-mount",
    ] {
        let mut query = command(root)
            .args([
                "explain",
                "--mountpoint",
                path,
                "--skill",
                "demo",
                "--format",
                "json",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let started = Instant::now();
        while query.try_wait().unwrap().is_none() {
            if started.elapsed() >= timeout {
                // Unblock any pending FUSE request before reaping the query.
                drop(resume);
                let _ = query.kill();
                query.wait().unwrap();
                panic!("explain waited for the paused worker on {path}");
            }
            thread::sleep(Duration::from_millis(10));
        }
        let output = query.wait_with_output().unwrap();
        assert!(output.status.success(), "{path}: {output:?}");
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap(),
            initial,
            "{path}"
        );
    }
}

#[test]
fn live_report_follows_the_serving_worker_across_configuration_drift_and_remount() {
    if !live_fuse_available() {
        return;
    }
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let root = temp.path();
    let a = root.join("A");
    let b = root.join("B");
    let mount = root.join("new-mount");
    let config_path = config(root, &[a.clone(), b.clone()]);
    for (source, marker) in [(&a, "from-A"), (&b, "from-B")] {
        skill(
            source,
            "demo",
            &format!("---\nname: demo\ndescription: fixture\n---\n{marker}\n"),
        );
        skill(source, "other", VALID);
    }
    skill(&a, "skill-discover", VALID);
    let secondary_demo = "[[view]]\nname = 'secondary'\nskills = ['demo']\n";
    fs::write(b.join("skillfs-views.toml"), secondary_demo).unwrap();
    let _stop = StopMount(root, &mount);
    let listed = || {
        fs::read_dir(mount.join("skills"))
            .unwrap()
            .any(|e| e.unwrap().file_name() == "demo")
    };
    start_mount(root, &config_path);
    let initial = runtime_report(root, "demo", 0);
    assert_eq!(initial["status"], "available");
    assert_eq!(initial["selected"]["source"], json!(a));
    assert_eq!(
        initial["shadowed"][0]["path"],
        json!(b.join("demo/SKILL.md"))
    );
    assert_eq!(initial["effective_default"], true);
    assert!(listed());
    let generated = runtime_report(root, "skill-discover", 0);
    assert_eq!(generated["selection_status"], "virtual");
    assert_eq!(generated["effective_default"], true);
    assert!(generated["selected"].is_null());
    assert_eq!(generated["shadowed"][0]["source"], json!(a));
    let missing = runtime_report(root, "missing", 0);
    assert_eq!(missing["selection_status"], "not_discovered");
    assert_eq!(missing["effective_default"], false);

    fs::write(a.join("skillfs-views.toml"), secondary_demo).unwrap();
    let disk = report(root, &config_path, &["--skill", "demo"], 0);
    assert_eq!(named(&disk, "demo")["effective_default"], false);
    assert!(listed());
    assert_eq!(runtime_report(root, "demo", 0), initial);

    config(root, &[b.clone(), a.clone()]);
    let disk = report(root, &config_path, &["--skill", "demo"], 0);
    assert_eq!(named(&disk, "demo")["selected"]["source"], json!(b));
    assert!(
        fs::read_to_string(mount.join("skills/demo/SKILL.md"))
            .unwrap()
            .contains("from-A")
    );
    assert_eq!(runtime_report(root, "demo", 0), initial);

    let id = initial["instance_id"].as_str().unwrap();
    let text = command(root)
        .args(["explain", "--mountpoint"])
        .arg(&mount)
        .args(["--skill", "demo"])
        .output()
        .unwrap();
    assert!(text.status.success());
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains(a.join("demo/SKILL.md").to_str().unwrap()));
    assert!(text.contains("Listed in default /skills: true"));
    let snapshot = root
        .join("runtime/skillfs")
        .join(format!("{id}.state.runtime.json"));
    let old_bytes = fs::read(&snapshot).unwrap();
    assert_eq!(
        fs::metadata(&snapshot).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(
        command(root)
            .arg("stop")
            .arg(&mount)
            .status()
            .unwrap()
            .success()
    );
    assert!(!snapshot.exists());
    assert_eq!(runtime_report(root, "demo", 1)["status"], "unavailable");
    start_mount(root, &config_path);
    let fresh = runtime_report(root, "demo", 0);
    assert_eq!(fresh["instance_id"], id);
    assert_ne!(initial["worker"], fresh["worker"]);
    assert_eq!(fresh["selected"]["source"], json!(b));
    assert_eq!(fresh["effective_default"], false);
    assert!(!listed());
    assert!(
        fs::read_to_string(mount.join("skills/demo/SKILL.md"))
            .unwrap()
            .contains("from-B")
    );

    // Even a complete receipt for the same mountpoint must not cross generations.
    fs::write(snapshot, old_bytes).unwrap();
    assert_eq!(runtime_report(root, "demo", 1)["status"], "unavailable");
}

#[test]
fn explicit_log_file_keeps_json_stdout_clean_without_ops_telemetry() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let source = temp.path().join("source");
    skill(&source, "demo", VALID);
    let config = config(temp.path(), &[source]);
    let log = temp.path().join("process.log");
    report(
        temp.path(),
        &config,
        &["--log-file", log.to_str().unwrap()],
        0,
    );
    assert!(!temp.path().join("ops.jsonl").exists());
}

#[test]
fn closed_stdout_fails_without_panicking_or_starting_telemetry() {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;

    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let source = temp.path().join("source");
    skill(&source, "demo", VALID);
    let config = config(temp.path(), &[source]);
    // Standard-library sockets are close-on-exec, so concurrent CLI tests
    // cannot inherit a reader and accidentally keep this output writable.
    let (reader, writer) = UnixStream::pair().unwrap();
    drop(reader);
    let output = command(temp.path())
        .arg("explain")
        .arg("--config")
        .arg(config)
        .args(["--format", "json"])
        .stdout(Stdio::from(OwnedFd::from(writer)))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("panicked"));
    assert!(!temp.path().join("ops.jsonl").exists());
}
