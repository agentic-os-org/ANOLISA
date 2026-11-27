//! Budget, inventory-cap, edit-comparison and Git-baseline observation tests.

use super::super::*;
use super::{candidate, observe, snapshot, workspace};
use std::fs::{self as stdfs, File};
use std::os::unix::fs::symlink;
use std::time::Duration;

use serde_json::json;
use tokio::time::Instant;

#[test]
fn timeout_keeps_the_original_deadline_and_does_not_read() {
    let dir = workspace();
    stdfs::write(dir.path().join("file"), "bytes").unwrap();
    let c = candidate("Write", json!({"path":"file","content":"bytes"}));
    let ingress = Ingress::capture().unwrap();
    let deadline = ingress.now_ms().saturating_sub(1);
    let result = collect(
        &c,
        Some(vec![("resource".into(), "file".into())]),
        dir.path().to_str().unwrap(),
        None,
        &revision(&c).unwrap(),
        ingress,
        deadline,
    );
    assert_eq!(result.valid_until_ms, deadline);
    assert!(result.files.is_empty());
    assert!(!result.fresh(ingress.now_ms()));
    assert!(result.gaps.contains(&"observation_timeout".into()));
    let inventory = Inventory::capture(dir.path().to_str().unwrap(), ingress, Instant::now());
    assert_eq!(serde_json::to_value(inventory).unwrap()["complete"], false);
}

#[test]
fn inventory_entry_and_serialized_byte_caps_are_explicit() {
    let dir = workspace();
    let parent = dir.path().join("p".repeat(250));
    stdfs::create_dir(&parent).unwrap();
    for index in 0..40 {
        File::create(parent.join(format!("{index:03}-{}", "x".repeat(246)))).unwrap();
    }
    let inventory = snapshot(&dir);
    let value = serde_json::to_value(&inventory).unwrap();
    assert_eq!(value["complete"], false);
    let entries = value["entries"].as_array().unwrap();
    assert!(entries.len() <= 256);
    assert!(
        entries
            .iter()
            .map(|e| e["path"].as_str().unwrap().len())
            .sum::<usize>()
            <= 32 * 1024
    );
    assert!(serde_json::to_vec(&inventory).unwrap().len() <= 20 * 1024);
    assert!(value["gaps"]
        .as_array()
        .unwrap()
        .contains(&json!("inventory_encoded_limit")));
    for index in 40..70 {
        File::create(parent.join(format!("{index:03}-{}", "x".repeat(246)))).unwrap();
    }
    let limited = snapshot(&dir);
    assert!(limited.gaps().any(|code| code == "inventory_path_bytes"));
    assert_eq!(
        limited.existed(&stdfs::metadata(dir.path()).unwrap(), "missing", None),
        None
    );
}

#[test]
fn inventory_count_exclusions_and_non_utf8_never_prove_absence() {
    use std::os::unix::ffi::OsStringExt;
    let dir = workspace();
    for parent in ["src", "tests"] {
        stdfs::create_dir(dir.path().join(parent)).unwrap();
        for index in 0..64 {
            File::create(dir.path().join(parent).join(index.to_string())).unwrap();
        }
    }
    let inventory = snapshot(&dir);
    let c = candidate("Write", json!({"path":"src/missing","content":"x"}));
    assert_eq!(
        observe(&dir, "src/missing", &c, Some(&inventory)).files[0].existed_at_start,
        None
    );
    let value = serde_json::to_value(inventory).unwrap();
    assert!(value["entries"].as_array().unwrap().len() <= 256);
    assert_eq!(value["complete"], false);
    assert!(value["gaps"]
        .as_array()
        .unwrap()
        .contains(&json!("inventory_path_limit")));
    let dir = workspace();
    let src = dir.path().join("src");
    stdfs::create_dir(&src).unwrap();
    for name in [".git", "node_modules", "target", ".venv"] {
        stdfs::create_dir(src.join(name)).unwrap();
        stdfs::write(src.join(name).join("unobserved"), "synthetic").unwrap();
    }
    File::create(src.join(std::ffi::OsString::from_vec(vec![0xff]))).unwrap();
    let value = serde_json::to_value(snapshot(&dir)).unwrap();
    assert_eq!(value["complete"], false);
    let entries = value["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["path"], "src");
    for code in ["inventory_excluded", "inventory_non_utf8"] {
        assert!(value["gaps"].as_array().unwrap().contains(&json!(code)));
    }
}

#[test]
fn parent_symlink_swap_never_reads_the_outside_fixture() {
    let dir = workspace();
    let outside = workspace();
    stdfs::create_dir(dir.path().join("branch")).unwrap();
    stdfs::write(dir.path().join("branch/file"), "inside fixture").unwrap();
    stdfs::write(outside.path().join("file"), "outside fixture").unwrap();
    let c = candidate(
        "Write",
        json!({"path":"branch/file","content":"inside fixture"}),
    );
    assert_eq!(
        observe(&dir, "branch/file", &c, None).files[0].delta,
        ContentDelta::Unchanged
    );
    let inside_hash = format!("sha256:{:x}", Sha256::digest(b"inside fixture"));
    std::thread::scope(|scope| {
        let branch = dir.path().join("branch");
        let held = dir.path().join("held");
        let outside = outside.path();
        scope.spawn(move || {
            for _ in 0..128 {
                stdfs::rename(&branch, &held).unwrap();
                symlink(outside, &branch).unwrap();
                std::thread::yield_now();
                stdfs::remove_file(&branch).unwrap();
                stdfs::rename(&held, &branch).unwrap();
            }
        });
        for _ in 0..128 {
            let result = observe(&dir, "branch/file", &c, None);
            for file in result.files {
                if let Some(hash) = file.sha256 {
                    assert_eq!(hash, inside_hash);
                }
            }
        }
    });
}

#[test]
fn exact_edit_comparison_matches_literal_replacement_and_honors_timeout() {
    let deadline = Instant::now() + Duration::from_secs(2);
    for text in ["", "a", "aaaaaa", "ababab", "aabbaabb", "ababa"] {
        for old in ["a", "aa", "ab", "aba", "b", "not present"] {
            for new in ["", "a", "ab", "aa", "different"] {
                let matches = text.matches(old).count();
                for all in [false, true] {
                    let expected = if matches == 0 || (!all && matches != 1) {
                        None
                    } else {
                        Some(text.replace(old, new) == text)
                    };
                    assert_eq!(
                        edited_equal(
                            text.as_bytes(),
                            old.as_bytes(),
                            new.as_bytes(),
                            all,
                            deadline
                        ),
                        expected
                    );
                }
            }
        }
    }
    assert_eq!(
        edited_equal(b"aaaa", b"a", b"a", true, Instant::now()),
        None
    );
}

#[test]
fn no_content_outside_the_candidate_and_no_escape_components() {
    let dir = workspace();
    stdfs::write(dir.path().join("untargeted"), "not observed").unwrap();
    let inventory = snapshot(&dir);
    let c = candidate("Write", json!({"path":"missing","content":"x"}));
    let result = observe(&dir, "missing", &c, Some(&inventory));
    assert_eq!(result.files.len(), 1);
    assert_eq!(result.files[0].state, FileState::Absent);
    assert!(!serde_json::to_string(&result)
        .unwrap()
        .contains("not observed"));
    for path in [
        "../escape",
        "nested/../../escape",
        "/unrelated/file",
        "a\0b",
    ] {
        assert!(fs::relative(path, dir.path().to_str().unwrap()).is_none());
    }
    assert!(fs::relative(&vec!["x"; 13].join("/"), dir.path().to_str().unwrap()).is_none());
}

#[test]
fn git_baseline_covers_large_repos_without_scan_budget() {
    let dir = workspace();
    let ws = dir.path().to_str().unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("/usr/bin/git")
            .args(["-C", ws])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap()
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "test@test"]);
    git(&["config", "user.name", "test"]);
    for index in 0..300 {
        stdfs::write(dir.path().join(format!("file{index}")), "content").unwrap();
    }
    git(&["add", "."]);
    git(&["commit", "-qm", "init"]);
    stdfs::write(dir.path().join("untracked"), "new").unwrap();
    stdfs::write(dir.path().join(".gitignore"), "ignored\n").unwrap();
    git(&["add", ".gitignore"]);
    git(&["commit", "-qm", "ignore"]);
    stdfs::write(dir.path().join("ignored"), "hidden").unwrap();

    let ingress = Ingress::capture().unwrap();
    let inventory = Inventory::capture(ws, ingress, ingress.preparation_deadline(5000));
    assert!(inventory.is_git());
    let value = serde_json::to_value(&inventory).unwrap();
    assert_eq!(value["complete"], true);
    assert_eq!(value["scope"], "recursive");
    assert!(value["git_head"].as_str().unwrap().len() == 40);
    assert!(value["entries"].as_array().unwrap().is_empty());

    let root = stdfs::metadata(dir.path()).unwrap();
    let deadline = ingress.preparation_deadline(5000);
    assert_eq!(
        inventory.existed_git(&root, ws, "file0", deadline),
        Some(true)
    );
    assert_eq!(
        inventory.existed_git(&root, ws, "file299", deadline),
        Some(true)
    );
    assert_eq!(
        inventory.existed_git(&root, ws, "untracked", deadline),
        Some(true)
    );
    assert_eq!(inventory.existed_git(&root, ws, "ignored", deadline), None);
    assert_eq!(inventory.existed_git(&root, ws, "absent", deadline), None);
    assert_eq!(inventory.existed(&root, "file0", None), None);

    let roundtrip: Inventory = serde_json::from_value(value).unwrap();
    assert!(roundtrip.is_git());
    assert_eq!(
        roundtrip.existed_git(&root, ws, "file150", deadline),
        Some(true)
    );
}
