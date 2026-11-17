use std::fs;

use super::file_protection_delete_tests::{identity, slot, Fixture, Worker};
use super::*;

/// Directory-tree read denial: reads, held descriptors, mappings, later
/// entries, link/rename exits and root exchange for one activated tree.
#[test]
#[ignore = "requires root, BPF LSM, unlimited memlock and --test-threads=1; private pins only"]
fn structured_tree_read_on_live_kernel() {
    let fixture = Fixture::new("fp-read");
    let tree = fixture.root.join("tree");
    let nested = tree.join("nested");
    let cache = tree.join("cache");
    let outside = fixture.root.join("outside");
    let decoy = fixture.root.join("decoy");
    for directory in [&nested, &cache, &outside, &decoy] {
        fs::create_dir_all(directory).expect("directory fixture");
    }
    let existing = tree.join("existing");
    let deep = nested.join("deep");
    let held = tree.join("held");
    let excluded = cache.join("excluded");
    let plain = outside.join("plain");
    let moved_out = tree.join("moved-out");
    for path in [&existing, &deep, &held, &excluded, &plain, &moved_out] {
        fs::write(path, b"fixture").expect("file fixture");
    }

    let engine = PinnedEngine::open_or_install_singleton().expect("load private engine");
    assert!(
        engine.supports_tree_read_guard(),
        "profile must attach the link guard"
    );
    let mut worker = Worker::new();
    engine
        .seed_label_in_domain(worker.pid(), 77, 1)
        .expect("seed worker domain");
    // Descriptors opened before activation are still mediated per read.
    assert_eq!(worker.run("hold", &held), "ok");

    let mut anchors =
        pinned_hash_map::<TreePathKey, PolicyRootVal>(&engine.paths, "te_policy_dirs")
            .expect("directory anchors");
    anchors
        .insert(identity(&tree), PolicyRootVal { root_mask: 1 }, BPF_NOEXIST)
        .expect("protect the tree");
    let mut slots =
        pinned_hash_map::<PolicySlotKey, PolicySlotVal>(&engine.paths, "te_policy_slots")
            .expect("slot map");
    slots
        .insert(
            slot(&fixture.root, "tree"),
            PolicySlotVal {
                direct_mask: 1,
                exact_exclude_mask: 0,
                subtree_exclude_mask: 0,
            },
            BPF_NOEXIST,
        )
        .expect("direct root slot");
    slots
        .insert(
            slot(&tree, "cache"),
            PolicySlotVal {
                direct_mask: 0,
                exact_exclude_mask: 0,
                subtree_exclude_mask: 1,
            },
            BPF_NOEXIST,
        )
        .expect("exclude the cache subtree");
    let mut domains = pinned_hash_map::<u32, PolicyDomainVal>(&engine.paths, "te_policy_domains")
        .expect("activation map");
    let mut domain = PolicyDomainVal {
        generation: 1,
        target_count: 1,
        tree_mask: 1,
        delete_mask: 0,
        ordinary_write_mask: 0,
        read_mask: 1,
        protection_set_id: 1,
        guard: InodeGuardVal {
            flags: INODE_GUARD_UNLINK | INODE_GUARD_RENAME,
            rule_id: 1,
            domain_id: 77,
            _pad: 0,
        },
    };
    domains
        .insert(77, domain, BPF_NOEXIST)
        .expect("activate the policy last");

    // Entries created, replaced or moved in after activation need no re-apply.
    let created = nested.join("created");
    fs::write(&created, b"created after apply").expect("new descendant");
    let moved_in = tree.join("moved-in");
    let incoming = outside.join("incoming");
    fs::write(&incoming, b"moved").expect("outside entry");
    fs::rename(&incoming, &moved_in).expect("unbound move into the tree");
    for path in [&existing, &deep, &created, &moved_in] {
        for operation in ["read", "mmapr"] {
            assert_eq!(
                worker.run(operation, path),
                "errno=1",
                "{operation} {}",
                path.display()
            );
        }
    }
    assert_eq!(worker.run("readheld", &held), "errno=1");
    // Read denial is independent: names stay listable and contents writable.
    assert_eq!(worker.run("listdir", &nested), "ok");
    assert_eq!(worker.run("write", &existing), "ok");
    for path in [&excluded, &plain] {
        assert_eq!(worker.run("read", path), "ok", "{}", path.display());
    }
    assert!(fs::read(&deep).is_ok(), "unbound processes keep access");

    // Second names and moves must not leave the protected scope.
    let alias = outside.join("alias");
    let link_alias = format!("link:{}", alias.display());
    assert_eq!(worker.run(&link_alias, &existing), "errno=1");
    assert!(!alias.exists());
    let excluded_alias = format!("link:{}", cache.join("alias").display());
    assert_eq!(worker.run(&excluded_alias, &existing), "errno=1");
    let inner_alias = format!("link:{}", nested.join("alias").display());
    assert_eq!(worker.run(&inner_alias, &existing), "ok");
    let rename_out = format!("rename:{}", outside.join("moved").display());
    assert_eq!(worker.run(&rename_out, &moved_out), "errno=1");
    assert!(moved_out.exists());
    let rename_dir_out = format!("rename:{}", outside.join("nested").display());
    assert_eq!(worker.run(&rename_dir_out, &nested), "errno=1");
    let rename_inside = format!("rename:{}", nested.join("moved").display());
    assert_eq!(worker.run(&rename_inside, &moved_out), "ok");
    let rename_in = format!("rename:{}", tree.join("plain").display());
    assert_eq!(worker.run(&rename_in, &plain), "ok");
    let exchange_root = format!("exchange:{}", decoy.display());
    assert_eq!(worker.run(&exchange_root, &tree), "errno=1");
    let exchange_out = format!("exchange:{}", outside.join("incoming-back").display());
    fs::write(outside.join("incoming-back"), b"outside").expect("exchange peer");
    assert_eq!(worker.run(&exchange_out, &existing), "errno=1");

    // Clearing the mask restores reads without detaching the domain.
    domain.read_mask = 0;
    domains
        .insert(77, domain, BPF_ANY)
        .expect("publish the mask change");
    assert_eq!(worker.run("read", &existing), "ok");
    assert_eq!(worker.run("readheld", &held), "ok");
    domains.remove(&77).expect("deactivate before cleanup");
}
