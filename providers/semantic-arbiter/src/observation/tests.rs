//! Real filesystem fixtures are synthetic, private and removed by TempDir.

use std::fs::{self as stdfs, File};
use std::os::unix::fs::symlink;
use std::time::Duration;

use semantic_arbiter::contract::{Candidate, Change, NativeTool, Operation};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::time::Instant;

use super::*;

mod limits;

fn workspace() -> TempDir {
    tempfile::tempdir().unwrap()
}

fn candidate(name: &str, input: Value) -> Candidate {
    Candidate {
        changes: vec![Change {
            operation: Operation::Replace,
            resource_id: "resource".into(),
        }],
        native_tool: Some(NativeTool {
            name: name.into(),
            input,
        }),
    }
}

fn snapshot(dir: &TempDir) -> Inventory {
    let ingress = Ingress::capture().unwrap();
    Inventory::capture(
        dir.path().to_str().unwrap(),
        ingress,
        ingress.preparation_deadline(2000),
    )
}

fn observe(
    dir: &TempDir,
    file: &str,
    candidate: &Candidate,
    inventory: Option<&Inventory>,
) -> LocalContext {
    let ingress = Ingress::capture().unwrap();
    let revision = revision(candidate).unwrap();
    let result = collect(
        candidate,
        Some(vec![(
            "resource".into(),
            dir.path().join(file).to_str().unwrap().into(),
        )]),
        dir.path().to_str().unwrap(),
        inventory,
        &revision,
        ingress,
        ingress.now_ms() + 2000,
    );
    result.validate(candidate, &revision).unwrap();
    result
}

#[test]
fn no_git_preexisting_and_created_files_are_distinguished() {
    let dir = workspace();
    for parent in ["src/deep", "tests/unit", "results/run"] {
        stdfs::create_dir_all(dir.path().join(parent)).unwrap();
    }
    let old = ["old", "src/deep/old", "tests/unit/old", "results/run/old"];
    let new = ["new", "src/deep/new", "tests/unit/new", "results/run/new"];
    for file in old {
        stdfs::write(dir.path().join(file), "synthetic old bytes").unwrap();
    }
    let encoded = serde_json::to_value(snapshot(&dir)).unwrap();
    assert_eq!(encoded["scope"], "recursive");
    assert_eq!(encoded["complete"], true);
    assert_eq!(encoded["gaps"], json!([]));
    let inventory: Inventory = serde_json::from_value(encoded).unwrap();
    for file in new {
        stdfs::write(dir.path().join(file), "synthetic new bytes").unwrap();
    }
    for (file, existed) in old
        .into_iter()
        .map(|p| (p, Some(true)))
        .chain(new.into_iter().map(|p| (p, Some(false))))
    {
        let candidate = candidate("Write", json!({"path":file,"content":"changed"}));
        let result = observe(&dir, file, &candidate, Some(&inventory));
        assert_eq!(result.files[0].existed_at_start, existed);
        assert_eq!(result.files[0].state, FileState::Regular);
        assert_eq!(result.files[0].delta, ContentDelta::Changed);
        assert!(result.files[0].sha256.is_some());
        let encoded = serde_json::to_string(&result).unwrap();
        assert!(!encoded.contains("synthetic"));
        assert!(!encoded.contains(dir.path().to_str().unwrap()));
    }
}

#[test]
fn absence_requires_complete_matching_scope_and_inventory_defaults_are_unknown() {
    let dir = workspace();
    stdfs::create_dir(dir.path().join("nested")).unwrap();
    let complete = snapshot(&dir);
    let unknown: Inventory = serde_json::from_value(json!({})).unwrap();
    let c = candidate("Write", json!({"path":"missing","content":"x"}));
    assert_eq!(
        observe(&dir, "missing", &c, Some(&complete)).files[0].existed_at_start,
        Some(false)
    );
    for inventory in [None, Some(&unknown)] {
        assert_eq!(
            observe(&dir, "missing", &c, inventory).files[0].existed_at_start,
            None
        );
    }
    let nested = candidate("Write", json!({"path":"nested/missing","content":"x"}));
    assert_eq!(
        observe(&dir, "nested/missing", &nested, Some(&complete)).files[0].existed_at_start,
        Some(false)
    );
    stdfs::create_dir(dir.path().join(".git")).unwrap();
    let incomplete = snapshot(&dir);
    assert_eq!(
        observe(&dir, "missing", &c, Some(&incomplete)).files[0].existed_at_start,
        None
    );
    let encoded = serde_json::to_value(&incomplete).unwrap();
    assert_eq!(encoded["complete"], false);
    assert_eq!(encoded["scope"], "recursive");
    assert!(encoded["gaps"]
        .as_array()
        .unwrap()
        .contains(&json!("inventory_excluded")));
    let other = workspace();
    assert_eq!(
        observe(&other, "missing", &c, Some(&complete)).files[0].existed_at_start,
        None
    );
    let root = stdfs::metadata(dir.path()).unwrap();
    for scope in ["unknown", "root_children", "recursive"] {
        let mut value = serde_json::to_value(&complete).unwrap();
        value["scope"] = json!(scope);
        // Even a persisted complete flag cannot erase recorded gaps.
        if scope == "recursive" {
            value["gaps"] = json!(["inventory_excluded"]);
        }
        let old: Inventory = serde_json::from_value(value).unwrap();
        for path in ["missing", "nested/missing"] {
            assert_eq!(old.existed(&root, path, None), None);
        }
    }
}

#[test]
fn replaced_inode_is_not_claimed_as_the_original_object() {
    let dir = workspace();
    stdfs::write(dir.path().join("file"), "old").unwrap();
    let inventory = snapshot(&dir);
    stdfs::write(dir.path().join("replacement"), "new").unwrap();
    stdfs::rename(dir.path().join("replacement"), dir.path().join("file")).unwrap();
    let c = candidate("Write", json!({"path":"file","content":"other"}));
    assert_eq!(
        observe(&dir, "file", &c, Some(&inventory)).files[0].existed_at_start,
        None
    );
}

#[test]
fn recursive_inventory_depth_limit_never_proves_absence() {
    let dir = workspace();
    let parent = ["deep"; 11].join("/");
    stdfs::create_dir_all(dir.path().join(&parent)).unwrap();
    let file = format!("{parent}/file");
    File::create(dir.path().join(&file)).unwrap();
    let complete = snapshot(&dir);
    let root = stdfs::metadata(dir.path()).unwrap();
    let meta = stdfs::metadata(dir.path().join(&file)).unwrap();
    assert_eq!(complete.existed(&root, &file, Some(&meta)), Some(true));
    assert_eq!(complete.existed(&root, "missing", None), Some(false));
    stdfs::create_dir(dir.path().join(&parent).join("limit")).unwrap();
    File::create(dir.path().join(&parent).join("limit/hidden")).unwrap();
    let limited = snapshot(&dir);
    assert_eq!(limited.existed(&root, "missing", None), None);
    assert_eq!(limited.existed(&root, &file, Some(&meta)), Some(true));
    assert!(limited.gaps().any(|code| code == "inventory_depth_limit"));
    let value = serde_json::to_value(limited).unwrap();
    assert!(value["entries"]
        .as_array()
        .unwrap()
        .iter()
        .all(|entry| entry["path"].as_str().unwrap().split('/').count() <= 12));
}

#[test]
fn unstable_directory_discards_its_subtree_but_not_stable_siblings() {
    for unstable in ["src/deep", "."] {
        let dir = workspace();
        stdfs::create_dir_all(dir.path().join("src/deep")).unwrap();
        for file in ["src/deep/old", "sibling"] {
            File::create(dir.path().join(file)).unwrap();
        }
        let ingress = Ingress::capture().unwrap();
        let inventory = Inventory::capture_with(
            dir.path().to_str().unwrap(),
            ingress,
            ingress.preparation_deadline(2000),
            |path| {
                if path == unstable {
                    File::create(dir.path().join(path).join("new")).unwrap();
                }
            },
        );
        assert!(
            inventory.gaps().any(|code| code == "inventory_changed"),
            "{unstable}: {inventory:?}"
        );
        let root = stdfs::metadata(dir.path()).unwrap();
        let old = stdfs::metadata(dir.path().join("src/deep/old")).unwrap();
        assert_eq!(inventory.existed(&root, "src/deep/old", Some(&old)), None);
        assert_eq!(inventory.existed(&root, "missing", None), None);
        let sibling = stdfs::metadata(dir.path().join("sibling")).unwrap();
        let expected = (unstable != ".").then_some(true);
        assert_eq!(
            inventory.existed(&root, "sibling", Some(&sibling)),
            expected
        );
        let value = serde_json::to_value(inventory).unwrap();
        assert!(value["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| !entry["path"].as_str().unwrap().starts_with("src/deep")));
    }
}

#[test]
fn replaced_workspace_root_rejects_even_the_same_file_inode() {
    for during_scan in [false, true] {
        let dir = workspace();
        let held = workspace();
        File::create(dir.path().join("file")).unwrap();
        let replace = || {
            stdfs::rename(dir.path(), held.path().join("original")).unwrap();
            stdfs::create_dir(dir.path()).unwrap();
            stdfs::hard_link(held.path().join("original/file"), dir.path().join("file")).unwrap();
        };
        let ingress = Ingress::capture().unwrap();
        let inventory = Inventory::capture_with(
            dir.path().to_str().unwrap(),
            ingress,
            ingress.preparation_deadline(2000),
            |path| {
                if during_scan && path == "." {
                    replace();
                }
            },
        );
        if !during_scan {
            replace();
        }
        let root = stdfs::metadata(dir.path()).unwrap();
        let file = stdfs::metadata(dir.path().join("file")).unwrap();
        assert_eq!(inventory.existed(&root, "file", Some(&file)), None);
        assert_eq!(inventory.existed(&root, "missing", None), None);
        if during_scan {
            let value = serde_json::to_value(inventory).unwrap();
            assert_eq!(value["complete"], false);
            assert_eq!(value["entries"], json!([]));
        }
    }
}

#[test]
fn literal_write_and_both_edit_dialects_compare_exact_bytes() {
    let dir = workspace();
    stdfs::write(dir.path().join("file"), b"a\r\na").unwrap();
    for (name, input, expected) in [
        (
            "Write",
            json!({"path":"file","content":"a\r\na"}),
            ContentDelta::Unchanged,
        ),
        (
            "write",
            json!({"file_path":"file","content":"a\na"}),
            ContentDelta::Changed,
        ),
        (
            "Edit",
            json!({"path":"file","old_string":"a","new_string":"a","replace_all":true}),
            ContentDelta::Unchanged,
        ),
        (
            "Edit",
            json!({"path":"file","old_string":"a","new_string":"b","replace_all":true}),
            ContentDelta::Changed,
        ),
        (
            "edit",
            json!({"path":"file","old_str":"\r\n","new_str":"\r\n"}),
            ContentDelta::Unchanged,
        ),
        (
            "edit",
            json!({"path":"file","old_str":"\r\n","new_str":"\n"}),
            ContentDelta::Changed,
        ),
        (
            "Edit",
            json!({"path":"file","old_string":"a","new_string":"a"}),
            ContentDelta::Unknown,
        ),
        (
            "Edit",
            json!({"path":"file","old_string":"missing","new_string":"x"}),
            ContentDelta::Unknown,
        ),
        (
            "Edit",
            json!({"path":"file","old_string":"","new_string":""}),
            ContentDelta::Unknown,
        ),
        (
            "Edit",
            json!({"path":"file","old_string":"a","new_string":"a","allow_multiple":true}),
            ContentDelta::Unknown,
        ),
        (
            "Edit",
            json!({"path":"file","old_string":"a","new_string":"a","old_str":"a","new_str":"a"}),
            ContentDelta::Unknown,
        ),
        (
            "Other",
            json!({"path":"file","content":"a\r\na"}),
            ContentDelta::Unknown,
        ),
        (
            "Write",
            json!({"path":"other","content":"a\r\na"}),
            ContentDelta::Unknown,
        ),
    ] {
        let c = candidate(name, input);
        assert_eq!(
            observe(&dir, "file", &c, None).files[0].delta,
            expected,
            "{c:?}"
        );
    }
    stdfs::write(dir.path().join("file"), [0xff, b'a', 0]).unwrap();
    let c = candidate(
        "Edit",
        json!({"path":"file","old_string":"a","new_string":"a"}),
    );
    assert_eq!(
        observe(&dir, "file", &c, None).files[0].delta,
        ContentDelta::Unchanged
    );
}

#[test]
fn symlinks_fifos_directories_and_oversize_files_never_get_hashes() {
    let dir = workspace();
    let outside = workspace();
    stdfs::write(outside.path().join("file"), "synthetic outside bytes").unwrap();
    symlink(outside.path().join("file"), dir.path().join("link")).unwrap();
    symlink(outside.path(), dir.path().join("parent")).unwrap();
    stdfs::create_dir(dir.path().join("directory")).unwrap();
    let fifo = std::ffi::CString::new(dir.path().join("fifo").to_str().unwrap()).unwrap();
    // SAFETY: fixture path is NUL-terminated and its private parent is live.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    File::create(dir.path().join("large"))
        .unwrap()
        .set_len(fs::FILE_LIMIT as u64 + 1)
        .unwrap();
    for (path, state) in [
        ("link", FileState::Symlink),
        ("parent/file", FileState::Unknown),
        ("fifo", FileState::Unknown),
        ("directory", FileState::Directory),
        ("large", FileState::Regular),
    ] {
        let c = candidate("Write", json!({"path":path,"content":"x"}));
        let result = observe(&dir, path, &c, None);
        assert_eq!(result.files[0].state, state, "{path}");
        assert_eq!(result.files[0].sha256, None, "{path}");
        assert_eq!(result.files[0].delta, ContentDelta::Unknown, "{path}");
    }
    let inventory = snapshot(&dir);
    let value = serde_json::to_value(&inventory).unwrap();
    assert_eq!(value["complete"], false);
    assert!(value["gaps"]
        .as_array()
        .unwrap()
        .contains(&json!("inventory_symlink")));
    assert!(value["entries"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| !e["path"].as_str().unwrap().starts_with("parent/")));
    // Replacing the link with a real directory cannot turn its unscanned
    // subtree into evidence of absence at task start.
    stdfs::remove_file(dir.path().join("parent")).unwrap();
    stdfs::create_dir(dir.path().join("parent")).unwrap();
    stdfs::write(dir.path().join("parent/file"), "new").unwrap();
    let c = candidate("Write", json!({"path":"parent/file","content":"x"}));
    assert_eq!(
        observe(&dir, "parent/file", &c, Some(&inventory)).files[0].existed_at_start,
        None
    );
}

#[test]
fn changing_file_and_replaced_name_fail_the_post_read_check() {
    let dir = workspace();
    let root = fs::root(
        dir.path().to_str().unwrap(),
        Instant::now() + Duration::from_secs(2),
    )
    .unwrap();
    for replacement in [false, true] {
        let path = dir.path().join("file");
        stdfs::write(&path, "before").unwrap();
        let before = fs::metadata(&root, "file").unwrap();
        let mut remaining = fs::TOTAL_LIMIT;
        let result = fs::read_stable(
            &root,
            "file",
            &before,
            &mut remaining,
            Instant::now() + Duration::from_secs(2),
            || {
                if replacement {
                    stdfs::write(dir.path().join("new"), "before").unwrap();
                    stdfs::rename(dir.path().join("new"), &path).unwrap();
                } else {
                    stdfs::write(&path, "different length").unwrap();
                }
            },
        );
        assert_eq!(result, Err("file_changed"));
        assert_eq!(remaining, fs::TOTAL_LIMIT - 6);
    }
}

#[test]
fn total_read_budget_and_target_binding_are_enforced() {
    let dir = workspace();
    let mut c = Candidate {
        changes: vec![],
        native_tool: None,
    };
    let mut targets = vec![];
    for index in 0..5 {
        let name = format!("f{index}");
        let path = dir.path().join(&name);
        File::create(&path)
            .unwrap()
            .set_len(fs::FILE_LIMIT as u64)
            .unwrap();
        c.changes.push(Change {
            resource_id: name.clone(),
            operation: Operation::Replace,
        });
        targets.push((name, path.to_str().unwrap().into()));
    }
    let ingress = Ingress::capture().unwrap();
    let rev = revision(&c).unwrap();
    let collect_targets = |targets, revision: &str| {
        collect(
            &c,
            targets,
            dir.path().to_str().unwrap(),
            None,
            revision,
            ingress,
            ingress.now_ms() + 2000,
        )
    };
    let result = collect_targets(Some(targets.clone()), &rev);
    assert_eq!(
        result.files.iter().filter(|f| f.sha256.is_some()).count(),
        4
    );
    assert!(result.gaps.contains(&"read_budget_exceeded".into()));
    result.validate(&c, &rev).unwrap();
    assert!(collect_targets(None, &rev).files.is_empty());
    assert!(collect_targets(Some(targets.clone()), "wrong")
        .files
        .is_empty());
    targets.swap(0, 1);
    assert!(collect_targets(Some(targets), &rev).files.is_empty());
}
