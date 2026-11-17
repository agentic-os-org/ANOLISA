use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::PathBuf;

use agentsight_enforcement_protocol::FileProtectionTargetKind;
use uuid::Uuid;

use super::super::file_protection_policy::NormalizedFileProtectionExclusion;
use super::*;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root =
            PathBuf::from("/dev/shm").join(format!("fp-tree-scan-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(root.join("tree/nested/deeper")).expect("tree fixture");
        fs::create_dir(root.join("outside")).expect("outside fixture");
        fs::write(root.join("tree/nested/deeper/secret"), b"fixture").expect("secret fixture");
        Self { root }
    }

    fn tree(&self) -> PathBuf {
        self.root.join("tree")
    }

    fn target(
        &self,
        exclusions: Vec<(&str, FileProtectionExclusionKind)>,
    ) -> NormalizedFileProtectionTarget {
        let tree = self.tree();
        NormalizedFileProtectionTarget {
            path: tree.to_str().expect("UTF-8 fixture").into(),
            kind: FileProtectionTargetKind::DirectoryTree,
            delete: false,
            ordinary_write: false,
            read: true,
            exclusions: exclusions
                .into_iter()
                .map(|(relative, kind)| NormalizedFileProtectionExclusion {
                    path: tree.join(relative).to_str().expect("UTF-8 fixture").into(),
                    kind,
                })
                .collect(),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn single_link_tree_and_symlinked_outside_links_are_accepted() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("outside/linked"), b"outside").expect("outside file");
    fs::hard_link(
        fixture.root.join("outside/linked"),
        fixture.root.join("outside/linked-twice"),
    )
    .expect("outside-only hard link");
    // Symlinks are not followed, so an outside multi-link file stays out of scope.
    symlink(fixture.root.join("outside"), fixture.tree().join("escape")).expect("symlink");
    let files = scan(
        &fixture.target(vec![]),
        &fixture.tree(),
        MAX_SCANNED_ENTRIES,
    )
    .expect("single-link tree must be accepted");
    let secret = fs::metadata(fixture.tree().join("nested/deeper/secret")).expect("secret");
    assert_eq!(
        files,
        [(userspace_dev_to_kernel(secret.dev()), secret.ino())].into(),
        "only in-scope regular files are recorded, never symlink targets"
    );
}

#[test]
fn excluded_entries_are_not_recorded_as_protected_identities() {
    let fixture = Fixture::new();
    fs::write(fixture.tree().join("public"), b"public").expect("excluded file");
    let files = scan(
        &fixture.target(vec![
            ("public", FileProtectionExclusionKind::ExactEntry),
            ("nested", FileProtectionExclusionKind::DirectorySubtree),
        ]),
        &fixture.tree(),
        MAX_SCANNED_ENTRIES,
    )
    .expect("tree with exclusions");
    assert!(files.is_empty(), "{files:?}");
}

#[test]
fn hard_linked_descendant_is_rejected_with_its_path() {
    let fixture = Fixture::new();
    let secret = fixture.tree().join("nested/deeper/secret");
    fs::hard_link(&secret, fixture.root.join("outside/alias")).expect("outside alias");
    let error = scan(
        &fixture.target(vec![]),
        &fixture.tree(),
        MAX_SCANNED_ENTRIES,
    )
    .expect_err("outside alias must reject apply");
    let message = error.to_string();
    assert!(message.contains("2 hard links"), "{message}");
    assert!(message.contains("nested/deeper/secret"), "{message}");
}

#[test]
fn exclusions_exempt_hard_linked_entries_with_their_scope() {
    let fixture = Fixture::new();
    let secret = fixture.tree().join("nested/deeper/secret");
    fs::hard_link(&secret, fixture.root.join("outside/alias")).expect("outside alias");
    for exclusion in [
        ("nested", FileProtectionExclusionKind::DirectorySubtree),
        (
            "nested/deeper/secret",
            FileProtectionExclusionKind::ExactEntry,
        ),
    ] {
        scan(
            &fixture.target(vec![exclusion]),
            &fixture.tree(),
            MAX_SCANNED_ENTRIES,
        )
        .expect("excluded entry is outside the read scope");
    }
    // An exact directory exclusion leaves its descendants protected.
    scan(
        &fixture.target(vec![("nested", FileProtectionExclusionKind::ExactEntry)]),
        &fixture.tree(),
        MAX_SCANNED_ENTRIES,
    )
    .expect_err("descendants of an exact directory exclusion stay in scope");
}

#[test]
fn oversized_tree_fails_closed() {
    let fixture = Fixture::new();
    let error = scan(&fixture.target(vec![]), &fixture.tree(), 2)
        .expect_err("scan budget must fail closed");
    assert!(error.to_string().contains("more than 2 entries"), "{error}");
}
