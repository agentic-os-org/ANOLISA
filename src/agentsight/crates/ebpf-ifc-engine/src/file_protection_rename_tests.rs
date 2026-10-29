use std::fs;

use super::file_protection_delete_tests::{identity, slot, Fixture, Worker};
use super::*;

/// Structured rename and exchange outcomes for one activated delete policy.
#[test]
#[ignore = "requires root, BPF LSM, unlimited memlock and --test-threads=1; private pins only"]
fn structured_rename_and_exchange_on_live_kernel() {
    let fixture = Fixture::new("fp-rename");
    let tree = fixture.root.join("tree");
    let cache = tree.join("cache");
    let outside = fixture.root.join("outside");
    fs::create_dir_all(&cache).expect("excluded subtree");
    fs::create_dir(&outside).expect("unprotected directory");
    let write_parent = fixture.root.join("write-parent");
    let write_tree = write_parent.join("write-tree");
    let write_peer = write_parent.join("peer");
    fs::create_dir_all(&write_tree).expect("write-only tree");
    fs::create_dir(&write_peer).expect("write-only exchange peer");
    let write_entry = write_tree.join("config");
    fs::write(&write_entry, b"write-only").expect("write-only descendant");
    let inside_source = tree.join("protected-entry");
    let inside_target = tree.join("entry-moved");
    let excluded_entry = cache.join("excluded");
    let outside_entry = outside.join("outside-entry");
    let outside_other = outside.join("other");
    let exact = fixture.root.join("exact");
    for path in [
        &inside_source,
        &excluded_entry,
        &outside_entry,
        &outside_other,
        &exact,
    ] {
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
    anchors
        .insert(
            identity(&write_tree),
            PolicyRootVal { root_mask: 4 },
            BPF_NOEXIST,
        )
        .expect("write-only tree anchor");
    let mut stable =
        pinned_hash_map::<TreePathKey, PolicyStableVal>(&engine.paths, "te_policy_stable")
            .expect("stable map");
    for path in [&write_tree, &write_parent] {
        stable
            .insert(
                identity(path),
                PolicyStableVal { target_mask: 4 },
                BPF_NOEXIST,
            )
            .expect("write-only ancestor");
    }
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
        .expect("exact stable target");
    let mut domains = pinned_hash_map::<u32, PolicyDomainVal>(&engine.paths, "te_policy_domains")
        .expect("activation map");
    domains
        .insert(
            77,
            PolicyDomainVal {
                generation: 1,
                target_count: 3,
                tree_mask: 5,
                delete_mask: 3,
                ordinary_write_mask: 4,
                protection_set_id: 1,
                guard: InodeGuardVal {
                    flags: INODE_GUARD_RENAME,
                    rule_id: 1,
                    domain_id: 77,
                    _pad: 0,
                },
            },
            BPF_NOEXIST,
        )
        .expect("activate the policy last");

    // Moving a protected entry out of its tree loses the protection, so it denies.
    assert_eq!(
        worker.run(
            &format!("rename:{}", outside.join("escaped").display()),
            &inside_source
        ),
        "errno=1"
    );
    assert!(
        inside_source.exists(),
        "denied rename must not move the entry"
    );
    // Replacing a protected destination denies even when the source is unprotected.
    assert_eq!(
        worker.run(
            &format!("rename:{}", inside_source.display()),
            &outside_entry
        ),
        "errno=1"
    );
    // An exact stable target cannot be renamed at all: its path must stay addressable.
    assert_eq!(
        worker.run(
            &format!("rename:{}", fixture.root.join("exact-moved").display()),
            &exact
        ),
        "errno=1"
    );
    assert!(exact.exists());
    // Exchanging a protected entry with an unprotected one denies in both directions.
    assert_eq!(
        worker.run(
            &format!("exchange:{}", outside_entry.display()),
            &inside_source
        ),
        "errno=1"
    );
    {
        engine
            .drain_pending_events()
            .expect("drain earlier denials");
        let mut ring = RingBuf::try_from(Map::RingBuf(
            pinned_map_data(&engine.paths, "rb").expect("private event ring"),
        ))
        .expect("open private event ring");
        assert_eq!(
            worker.run(
                &format!("exchange:{}", inside_source.display()),
                &outside_entry
            ),
            "errno=1"
        );
        assert!(ring_readable(ring.as_raw_fd(), 3000).expect("wait for exchange denial"));
        let item = ring.next().expect("exchange denial event");
        let bytes: &[u8] = &item;
        assert!(bytes.len() >= std::mem::size_of::<Event>());
        // SAFETY: the size check covers the complete repr(C) kernel event.
        let raw: Event = unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<Event>()) };
        assert_eq!(raw.etype, EVENT_TYPE_TAINT_VIOLATION);
        let event = decode(&raw);
        assert_eq!(event.op, 6);
        assert!(event.blocked);
        assert_eq!(event.pid, worker.pid());
        assert_eq!(event.target, "protected-entry");
    }
    assert_eq!(worker.run("write", &write_entry), "errno=1");
    let moved_write_tree = write_parent.join("moved-tree");
    for (source, destination) in [
        (&write_tree, &moved_write_tree),
        (&moved_write_tree, &write_tree),
    ] {
        let before = identity(source).ino;
        assert_eq!(
            worker.run(&format!("rename:{}", destination.display()), source),
            "ok"
        );
        assert!(!source.exists());
        assert_eq!(identity(destination).ino, before);
    }
    let tree_inode = identity(&write_tree).ino;
    let peer_inode = identity(&write_peer).ino;
    assert_eq!(
        worker.run(&format!("exchange:{}", write_peer.display()), &write_tree),
        "ok"
    );
    assert_eq!(identity(&write_tree).ino, peer_inode);
    assert_eq!(identity(&write_peer).ino, tree_inode);
    assert_eq!(
        worker.run(&format!("exchange:{}", write_peer.display()), &write_tree),
        "ok"
    );
    assert_eq!(identity(&write_tree).ino, tree_inode);
    let moved_write_parent = fixture.root.join("moved-parent");
    for (source, destination) in [
        (&write_parent, &moved_write_parent),
        (&moved_write_parent, &write_parent),
    ] {
        let before = identity(source).ino;
        assert_eq!(
            worker.run(&format!("rename:{}", destination.display()), source),
            "ok"
        );
        assert!(!source.exists());
        assert_eq!(identity(destination).ino, before);
    }
    assert_eq!(worker.run("write", &write_entry), "errno=1");

    // Renames that stay inside one protected tree remain allowed.
    assert_eq!(
        worker.run(
            &format!("rename:{}", inside_target.display()),
            &inside_source
        ),
        "ok"
    );
    assert!(inside_target.exists() && !inside_source.exists());
    // Excluded and unrelated entries keep their ordinary rename behavior.
    assert_eq!(
        worker.run(
            &format!("rename:{}", cache.join("moved").display()),
            &excluded_entry
        ),
        "ok"
    );
    assert_eq!(
        worker.run(
            &format!("rename:{}", outside.join("moved").display()),
            &outside_entry
        ),
        "ok"
    );
    assert_eq!(
        worker.run(
            &format!("exchange:{}", outside_other.display()),
            &outside.join("moved")
        ),
        "ok"
    );

    domains.remove(&77).expect("deactivate before cleanup");
    assert_eq!(
        worker.run(
            &format!("rename:{}", inside_source.display()),
            &inside_target
        ),
        "ok"
    );
}
