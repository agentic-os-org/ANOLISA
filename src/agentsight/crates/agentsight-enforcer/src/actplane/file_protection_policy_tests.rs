use agentsight_enforcement_protocol::{
    FILE_PROTECTION_POLICY_VERSION, FileProtectionExclusion, FileProtections,
};

use super::*;

fn target(
    path: &str,
    kind: FileProtectionTargetKind,
    delete: bool,
    ordinary_write: bool,
    exclusions: Vec<FileProtectionExclusion>,
) -> FileProtectionTarget {
    FileProtectionTarget {
        path: path.into(),
        kind,
        protections: FileProtections {
            delete,
            ordinary_write,
        },
        exclusions,
    }
}

fn exclusion(path: &str, kind: FileProtectionExclusionKind) -> FileProtectionExclusion {
    FileProtectionExclusion {
        path: path.into(),
        kind,
    }
}

fn policy(targets: Vec<FileProtectionTarget>) -> FileProtectionPolicy {
    FileProtectionPolicy {
        version: FILE_PROTECTION_POLICY_VERSION,
        topology_mode: FileProtectionTopologyMode::StaticAtApply,
        targets,
    }
}

#[test]
fn t1_normalizes_an_exact_file_without_touching_its_path() {
    let normalized = normalize(&policy(vec![target(
        "/srv/config.json",
        FileProtectionTargetKind::ExactFile,
        true,
        false,
        vec![],
    )]))
    .expect("exact policy should normalize");

    assert_eq!(
        normalized.topology_mode,
        FileProtectionTopologyMode::StaticAtApply
    );
    let exact = normalized
        .targets
        .iter()
        .find(|entry| entry.path == "/srv/config.json")
        .expect("exact target should remain represented");
    assert_eq!(exact.kind, FileProtectionTargetKind::ExactFile);
    assert!(exact.delete);
    assert!(!exact.ordinary_write);
    assert!(exact.exclusions.is_empty());
}

#[test]
fn t2_normalizes_a_directory_tree_and_scoped_exclusion() {
    let normalized = normalize(&policy(vec![target(
        "/srv/tree",
        FileProtectionTargetKind::DirectoryTree,
        true,
        true,
        vec![exclusion(
            "/srv/tree/socket",
            FileProtectionExclusionKind::ExactEntry,
        )],
    )]))
    .expect("tree policy should normalize");
    let tree = &normalized.targets[0];

    assert_eq!(tree.kind, FileProtectionTargetKind::DirectoryTree);
    assert!(tree.delete && tree.ordinary_write);
    assert_eq!(tree.exclusions[0].path, "/srv/tree/socket");
    assert_eq!(
        tree.exclusions[0].kind,
        FileProtectionExclusionKind::ExactEntry
    );
}

#[test]
fn t3_merges_identical_targets_and_keeps_overlaps_deterministic() {
    let shared_exclusions = vec![
        exclusion(
            "/workspace/cache",
            FileProtectionExclusionKind::DirectorySubtree,
        ),
        exclusion("/workspace/socket", FileProtectionExclusionKind::ExactEntry),
    ];
    let targets = vec![
        target(
            "/workspace/project/config",
            FileProtectionTargetKind::ExactFile,
            true,
            false,
            vec![],
        ),
        target(
            "/workspace",
            FileProtectionTargetKind::DirectoryTree,
            false,
            true,
            shared_exclusions.clone(),
        ),
        target(
            "/workspace",
            FileProtectionTargetKind::DirectoryTree,
            true,
            false,
            vec![shared_exclusions[1].clone(), shared_exclusions[0].clone()],
        ),
    ];
    let mut reordered = targets.clone();
    reordered.reverse();

    let normalized = normalize(&policy(targets)).expect("overlaps should be representable");
    assert_eq!(
        normalized,
        normalize(&policy(reordered)).expect("input order must not affect normalized policy")
    );
    let root = normalized
        .targets
        .iter()
        .find(|entry| entry.path == "/workspace")
        .expect("duplicate tree target should merge");
    assert!(root.delete && root.ordinary_write);
    assert!(
        normalized
            .targets
            .iter()
            .any(|entry| entry.path == "/workspace/project/config"),
        "an overlapping direct target must remain explicit"
    );
    assert!(
        root.exclusions
            .windows(2)
            .all(|pair| pair[0].path < pair[1].path)
    );
}

#[test]
fn t4_deduplicates_identical_exact_entry_exclusions() {
    let item = exclusion(
        "/workspace/runtime.sock",
        FileProtectionExclusionKind::ExactEntry,
    );
    let normalized = normalize(&policy(vec![target(
        "/workspace",
        FileProtectionTargetKind::DirectoryTree,
        true,
        false,
        vec![item.clone(), item],
    )]))
    .expect("identical exclusions should deduplicate");

    assert_eq!(
        normalized.targets[0].exclusions,
        vec![NormalizedFileProtectionExclusion {
            path: "/workspace/runtime.sock".into(),
            kind: FileProtectionExclusionKind::ExactEntry,
        }]
    );
}

#[test]
fn t5_retains_future_subtrees_and_direct_targets_under_exclusions() {
    let normalized = normalize(&policy(vec![
        target(
            "/workspace",
            FileProtectionTargetKind::DirectoryTree,
            true,
            false,
            vec![exclusion(
                "/workspace/generated/future",
                FileProtectionExclusionKind::DirectorySubtree,
            )],
        ),
        target(
            "/workspace/generated/future/manifest.json",
            FileProtectionTargetKind::ExactFile,
            false,
            true,
            vec![],
        ),
    ]))
    .expect("explicit targets may override an ancestor exclusion later");

    let ancestor = normalized
        .targets
        .iter()
        .find(|entry| entry.path == "/workspace")
        .expect("ancestor should remain represented");
    assert!(ancestor.exclusions.iter().any(|entry| {
        entry.path == "/workspace/generated/future"
            && entry.kind == FileProtectionExclusionKind::DirectorySubtree
    }));
    assert!(normalized.targets.iter().any(|entry| {
        entry.path == "/workspace/generated/future/manifest.json" && entry.ordinary_write
    }));
}

#[test]
fn invalid_target_and_exclusion_paths_report_their_payload_positions() {
    let invalid = [
        "relative",
        "",
        "/bad\0path",
        "/bad*path",
        "/bad//path",
        "/bad/./path",
        "/bad/../path",
        "/bad/path/",
    ];
    for path in invalid {
        let target_failure = normalize(&policy(vec![target(
            path,
            FileProtectionTargetKind::ExactFile,
            true,
            false,
            vec![],
        )]))
        .expect_err("invalid target path must fail");
        assert_eq!(target_failure.target_index, 0);
        assert_eq!(target_failure.exclusion_index, None);
        assert_eq!(target_failure.path, format!("{path:?}"));

        let exclusion_failure = normalize(&policy(vec![target(
            "/root",
            FileProtectionTargetKind::DirectoryTree,
            true,
            false,
            vec![exclusion(path, FileProtectionExclusionKind::ExactEntry)],
        )]))
        .expect_err("invalid exclusion path must fail");
        assert_eq!(exclusion_failure.target_index, 0);
        assert_eq!(exclusion_failure.exclusion_index, Some(0));
        assert_eq!(exclusion_failure.path, format!("{path:?}"));
    }
}

#[test]
fn exclusions_must_be_strict_component_descendants() {
    for path in ["/root", "/rooted/item", "/outside/item"] {
        let error = normalize(&policy(vec![target(
            "/root",
            FileProtectionTargetKind::DirectoryTree,
            true,
            false,
            vec![exclusion(path, FileProtectionExclusionKind::ExactEntry)],
        )]))
        .expect_err("equal or outside exclusions must fail");
        assert_eq!(error.exclusion_index, Some(0));
        assert!(error.to_string().contains(&format!("{path:?}")));
    }
    assert!(
        normalize(&policy(vec![target(
            "/",
            FileProtectionTargetKind::DirectoryTree,
            true,
            false,
            vec![exclusion("/child", FileProtectionExclusionKind::ExactEntry)],
        )]))
        .is_ok()
    );
}

#[test]
fn exact_files_cannot_own_exclusions() {
    let error = normalize(&policy(vec![target(
        "/root/file",
        FileProtectionTargetKind::ExactFile,
        true,
        false,
        vec![exclusion(
            "/root/file/child",
            FileProtectionExclusionKind::ExactEntry,
        )],
    )]))
    .expect_err("exact target exclusions must fail");

    assert_eq!(error.target_index, 0);
    assert_eq!(error.exclusion_index, Some(0));
    assert!(error.to_string().contains("cannot have exclusions"));
}

#[test]
fn same_exclusion_path_with_different_kinds_conflicts() {
    let error = normalize(&policy(vec![target(
        "/root",
        FileProtectionTargetKind::DirectoryTree,
        true,
        false,
        vec![
            exclusion("/root/cache", FileProtectionExclusionKind::ExactEntry),
            exclusion("/root/cache", FileProtectionExclusionKind::DirectorySubtree),
        ],
    )]))
    .expect_err("ambiguous exclusion kind must fail");

    assert_eq!(error.exclusion_index, Some(1));
    assert!(error.to_string().contains("exclusion 0"));
}

#[test]
fn duplicate_target_path_with_different_kinds_conflicts() {
    let error = normalize(&policy(vec![
        target(
            "/root/item",
            FileProtectionTargetKind::ExactFile,
            true,
            false,
            vec![],
        ),
        target(
            "/root/item",
            FileProtectionTargetKind::DirectoryTree,
            true,
            false,
            vec![],
        ),
    ]))
    .expect_err("ambiguous target kind must fail");

    assert_eq!(error.target_index, 1);
    assert!(error.to_string().contains("target 0"));
}

#[test]
fn normalized_target_ids_are_bounded_to_the_kernel_mask() {
    let targets = (0..65)
        .map(|index| {
            target(
                &format!("/root/item-{index}"),
                FileProtectionTargetKind::ExactFile,
                true,
                false,
                vec![],
            )
        })
        .collect();
    let error = normalize(&policy(targets)).expect_err("65 unique targets exceed the ABI mask");
    assert_eq!(error.target_index, 64);
    assert!(error.to_string().contains("at most 64 normalized targets"));

    let duplicates = (0..65)
        .map(|_| {
            target(
                "/root/same",
                FileProtectionTargetKind::ExactFile,
                true,
                false,
                vec![],
            )
        })
        .collect();
    assert_eq!(
        normalize(&policy(duplicates))
            .expect("duplicates share one stable target id")
            .targets
            .len(),
        1
    );
}

#[test]
fn duplicate_targets_require_identical_normalized_exclusions() {
    let error = normalize(&policy(vec![
        target(
            "/root",
            FileProtectionTargetKind::DirectoryTree,
            true,
            false,
            vec![exclusion(
                "/root/a",
                FileProtectionExclusionKind::ExactEntry,
            )],
        ),
        target(
            "/root",
            FileProtectionTargetKind::DirectoryTree,
            false,
            true,
            vec![exclusion(
                "/root/b",
                FileProtectionExclusionKind::ExactEntry,
            )],
        ),
    ]))
    .expect_err("divergent exclusion scopes must fail");

    assert_eq!(error.target_index, 1);
    assert!(
        error
            .to_string()
            .contains("different normalized exclusions")
    );
}
