use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};

use super::super::file_protection_manifest::tests::{Fixture, fixture_root, mount};
use super::super::file_protection_resolver::{open_held, open_held_directory};
use super::*;

fn openat2_available(root: &std::fs::File) -> bool {
    !matches!(open_held(root, "/"), Err(error) if error.raw_errno() == Some(libc::ENOSYS))
}

#[test]
fn lexical_helpers_respect_component_boundaries() {
    assert_eq!(
        lexical_ancestors("/tree/sub/deep"),
        vec!["/tree/sub/deep", "/tree/sub", "/tree"]
    );
    assert_eq!(lexical_ancestors("/tree"), vec!["/tree"]);

    assert!(is_component_descendant("/tree", "/tree/sub"));
    assert!(is_component_descendant("/", "/tree"));
    // A shared string prefix is not a path relation.
    assert!(!is_component_descendant("/tree", "/treeish"));
    assert!(!is_component_descendant("/tree", "/tree"));
    assert!(!is_component_descendant("/", "/"));

    assert!(path_in_scope("/tree", "/tree"));
    assert!(path_in_scope("/tree", "/tree/sub"));
    assert!(!path_in_scope("/tree", "/treeish"));
}

#[test]
fn submount_discovery_matches_resolved_identity_not_authored_spelling() {
    let fixture = Fixture::new("fp-discovery");
    fixture.mkdir("/real/tree/mounted");
    symlink("real", fixture.host("/alias")).expect("intermediate directory symlink");
    let root = fixture_root(&fixture.root);
    if !openat2_available(&root) {
        eprintln!("skipping: host kernel does not provide openat2");
        return;
    }
    let target = open_held_directory(&root, "/alias/tree").expect("hold authored alias");
    let mounts = vec![
        mount(1, 0, "/", "/", "ext4"),
        mount(2, 1, "/outside/source", "/real/tree/mounted", "ext4"),
    ];

    let discovery = discover_submounts(&root, &mounts, "/alias/tree", target.identity)
        .expect("discover canonical submount through an alias");

    // The submount is below the target's canonical spelling, not its authored one.
    assert_eq!(discovery.indices, vec![1]);
    assert!(discovery.target_spellings.contains("/alias/tree"));
    assert!(discovery.target_spellings.contains("/real/tree"));
}

#[test]
fn submount_discovery_ignores_unrelated_and_unresolvable_mounts() {
    let fixture = Fixture::new("fp-discovery");
    fixture.mkdir("/tree");
    fixture.mkdir("/treeish/sub");
    let root = fixture_root(&fixture.root);
    if !openat2_available(&root) {
        eprintln!("skipping: host kernel does not provide openat2");
        return;
    }
    let target = open_held_directory(&root, "/tree").expect("hold target");

    let discovery = discover_submounts(
        &root,
        &[
            mount(1, 0, "/", "/", "ext4"),
            // A shared prefix outside the target subtree.
            mount(2, 1, "/other", "/treeish/sub", "ext4"),
            // An unrelated mountpoint that cannot be resolved at all.
            mount(3, 1, "/missing", "/hidden/missing", "ext4"),
        ],
        "/tree",
        target.identity,
    )
    .expect("unrelated mounts must not reject the policy");
    assert!(discovery.indices.is_empty());
    assert!(discovery.aliases.is_empty());

    // An unresolvable mountpoint inside the protected subtree still fails closed.
    let error = discover_submounts(
        &root,
        &[
            mount(1, 0, "/", "/", "ext4"),
            mount(2, 1, "/missing", "/tree/missing", "ext4"),
        ],
        "/tree",
        target.identity,
    )
    .expect_err("a protected submount must resolve");
    assert!(matches!(error, BackendError::CompileFailure(_)));
}

#[test]
fn underlying_submount_paths_keep_only_matching_directory_sources() {
    let fixture = Fixture::new("fp-discovery");
    fixture.mkdir("/outside/source");
    fixture.mkdir("/tree/mounted");
    fixture.write("/outside/decoy");
    let root = fixture_root(&fixture.root);
    if !openat2_available(&root) {
        eprintln!("skipping: host kernel does not provide openat2");
        return;
    }
    // Model a bind mount by pointing the mounted view at the backing directory.
    let mounted = open_held_directory(&root, "/outside/source").expect("hold backing source");
    let mounts = vec![
        mount(1, 0, "/", "/", "ext4"),
        mount(2, 1, "/outside/source", "/tree/mounted", "ext4"),
    ];

    let underlying = underlying_submount_paths(&root, &mounts, &mounts[1], &mounted);

    assert_eq!(underlying.len(), 1);
    assert_eq!(underlying[0].authored_path, "/outside/source");
    assert_eq!(
        underlying[0].identity.ino,
        fs::metadata(fixture.host("/outside/source"))
            .expect("metadata")
            .ino()
    );

    // A backing path whose identity differs from the mounted view is not adopted.
    let other = open_held_directory(&root, "/tree/mounted").expect("hold mounted view");
    assert!(underlying_submount_paths(&root, &mounts, &mounts[1], &other).is_empty());
}
