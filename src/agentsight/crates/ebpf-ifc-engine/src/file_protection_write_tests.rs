use std::fs;

use super::file_protection_delete_tests::{identity, slot, Fixture, Worker};
use super::*;

/// Ordinary write denial for one activated write-protection policy.
#[test]
#[ignore = "requires root, BPF LSM, unlimited memlock and --test-threads=1; private pins only"]
fn structured_ordinary_write_on_live_kernel() {
    let fixture = Fixture::new("fp-write");
    let tree = fixture.root.join("tree");
    let cache = tree.join("cache");
    fs::create_dir_all(&cache).expect("excluded subtree");
    let inside = tree.join("entry");
    let excluded = cache.join("excluded");
    let exact = fixture.root.join("exact");
    let outside = fixture.root.join("outside");
    let deletable = tree.join("deletable");
    for path in [&inside, &excluded, &exact, &outside, &deletable] {
        fs::write(path, b"fixture").expect("file fixture");
    }

    let engine = PinnedEngine::open_or_install_singleton().expect("load private engine");
    let mut worker = Worker::new();
    engine
        .seed_label_in_domain(worker.pid(), 77, 1)
        .expect("seed worker domain");
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
            slot(&tree, "cache"),
            PolicySlotVal {
                direct_mask: 0,
                exact_exclude_mask: 0,
                subtree_exclude_mask: 1,
            },
            BPF_NOEXIST,
        )
        .expect("exclude the cache subtree");
    slots
        .insert(
            slot(&fixture.root, "exact"),
            PolicySlotVal {
                direct_mask: 2,
                exact_exclude_mask: 0,
                subtree_exclude_mask: 0,
            },
            BPF_NOEXIST,
        )
        .expect("exact write target");
    let mut domains = pinned_hash_map::<u32, PolicyDomainVal>(&engine.paths, "te_policy_domains")
        .expect("activation map");
    let mut domain = PolicyDomainVal {
        generation: 1,
        target_count: 2,
        tree_mask: 1,
        // Write-only: no delete protection is requested by this policy.
        delete_mask: 0,
        ordinary_write_mask: 3,
        protection_set_id: 1,
        guard: InodeGuardVal {
            flags: INODE_GUARD_WRITE,
            rule_id: 1,
            domain_id: 77,
            _pad: 0,
        },
    };
    domains
        .insert(77, domain, BPF_NOEXIST)
        .expect("activate the policy last");

    for path in [&inside, &exact] {
        assert_eq!(worker.run("write", path), "errno=1", "{}", path.display());
        assert_eq!(
            fs::read(path).expect("protected file stays readable"),
            b"fixture",
            "denied write must not change content"
        );
    }
    for path in [&excluded, &outside] {
        assert_eq!(worker.run("write", path), "ok", "{}", path.display());
    }
    // A write-only policy must not turn deletions into denials.
    assert_eq!(worker.run("unlink", &deletable), "ok");
    assert_eq!(worker.run("rmdir", &cache.join("missing")), "errno=2");

    // Switching the mask off restores ordinary write behavior.
    domain.ordinary_write_mask = 0;
    domains
        .insert(77, domain, BPF_ANY)
        .expect("publish the mask change");
    assert_eq!(worker.run("write", &inside), "ok");

    domains.remove(&77).expect("deactivate before cleanup");
    assert_eq!(worker.run("write", &exact), "ok");
}
