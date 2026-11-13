//! Restore isolation through independent MCP server processes.

#[path = "common/mod.rs"]
mod common;

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use common::McpAgent;
use nix::fcntl::{Flock, FlockArg};
use serde_json::json;

const ENV: &[(&str, &str)] = &[
    ("MEMORY_INDEX_ENABLED", "false"),
    ("MEMORY_CONSOLIDATION_ENABLED", "false"),
    ("MEMORY_CGROUP_ENABLED", "false"),
    ("MEMORY_SNAPSHOT_RECLAIM_ENABLED", "true"),
];

fn tree(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .filter_entry(|entry| entry.file_name() != ".anolisa")
        .map(|entry| {
            let entry = entry.unwrap();
            let content = entry
                .file_type()
                .is_file()
                .then(|| std::fs::read(entry.path()).unwrap());
            (entry.path().strip_prefix(root).unwrap().to_owned(), content)
        })
        .collect()
}

async fn concurrent_restores(same_snapshot: bool) {
    let tmp = tempfile::tempdir().unwrap();
    let mut a = McpAgent::spawn(tmp.path(), ENV).await;
    let mut b = McpAgent::spawn(tmp.path(), ENV).await;
    let root = tmp.path().join("user-tester");
    for i in 0..80 {
        let dir = root.join(format!("a{i}"));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("f.md"), format!("A-{i}").repeat(1024)).unwrap();
    }
    std::fs::write(root.join("shared.txt"), "A").unwrap();
    let tree_a = tree(&root);
    let snap_a = a.call_json("mem_snapshot", json!({})).await;
    for i in 0..80 {
        std::fs::remove_dir_all(root.join(format!("a{i}"))).unwrap();
        let dir = root.join(format!("b{i}"));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("f.md"), format!("B-{i}").repeat(1024)).unwrap();
    }
    std::fs::write(root.join("shared.txt"), "B").unwrap();
    let tree_b = tree(&root);
    let snap_b = b.call_json("mem_snapshot", json!({})).await;
    let id_a = snap_a["id"].as_str().unwrap();
    let id_b = if same_snapshot {
        id_a
    } else {
        snap_b["id"].as_str().unwrap()
    };

    let mut displaced = Vec::new();
    for round in 0..if same_snapshot { 2 } else { 1 } {
        if same_snapshot {
            std::fs::write(root.join("shared.txt"), format!("round-{round}")).unwrap();
            displaced.push(tree(&root));
        }
        let (result_a, result_b) = tokio::join!(
            a.call("mem_snapshot_restore", json!({"id": id_a})),
            b.call("mem_snapshot_restore", json!({"id": id_b})),
        );
        let success_a = result_a == format!("restored {id_a}");
        let success_b = result_b == format!("restored {id_b}");
        for (success, result) in [(success_a, &result_a), (success_b, &result_b)] {
            assert!(
                success || result.contains("another snapshot restore or reclaim is in progress"),
                "unexpected restore result: {result}"
            );
        }
        assert!(success_a || success_b, "both restores failed");
        let actual = tree(&root);
        assert!(
            ((success_a || (same_snapshot && success_b)) && actual == tree_a)
                || (!same_snapshot && success_b && actual == tree_b),
            "successful restores left an incomplete or mixed tree: {result_a}; {result_b}"
        );
        if same_snapshot && success_a && success_b {
            // Both may succeed sequentially if the first finishes before the second locks.
            displaced.push(tree_a.clone());
        }
    }
    if same_snapshot {
        std::fs::write(root.join("shared.txt"), "single restore").unwrap();
        displaced.push(tree(&root));
        assert_eq!(
            a.call("mem_snapshot_restore", json!({"id": id_a})).await,
            format!("restored {id_a}")
        );
        assert_eq!(tree(&root), tree_a);
        let mut retained: Vec<_> = std::fs::read_dir(root.join(".anolisa/trash"))
            .unwrap()
            .map(|entry| tree(&entry.unwrap().path()))
            .collect();
        retained.sort();
        displaced.sort();
        assert_eq!(retained, displaced);
        assert!(
            std::fs::read_dir(root.join(".anolisa"))
                .unwrap()
                .all(|entry| {
                    !entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(&format!(".{id_a}.rollback."))
                })
        );
    }
    a.cleanup().await;
    b.cleanup().await;
}

#[tokio::test]
async fn two_servers_restore_the_same_snapshot() {
    for _ in 0..8 {
        concurrent_restores(true).await;
    }
}

#[tokio::test]
async fn two_servers_restore_different_snapshots() {
    for _ in 0..8 {
        concurrent_restores(false).await;
    }
}

#[tokio::test]
async fn busy_restore_preserves_live_and_staging_files_then_retries() {
    let tmp = tempfile::tempdir().unwrap();
    let mut agent = McpAgent::spawn(tmp.path(), ENV).await;
    let root = tmp.path().join("user-tester");
    std::fs::write(root.join("doc.md"), "snapshot").unwrap();
    let snapshot = agent.call_json("mem_snapshot", json!({})).await;
    let id = snapshot["id"].as_str().unwrap();
    std::fs::write(root.join("doc.md"), "live").unwrap();
    let staging = root.join(format!(".anolisa/snapshots/.{id}.staging"));
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(staging.join("active.md"), "in progress").unwrap();
    File::open(&staging)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
        .unwrap();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(".anolisa/.restore.lock"))
        .unwrap();
    let lock = Flock::<File>::lock(file, FlockArg::LockExclusiveNonblock).unwrap();
    // Opening another mount runs reclaim. Even old staging belongs to the
    // lock holder until it releases the lock, not to the startup sweeper.
    let mut other = McpAgent::spawn(tmp.path(), ENV).await;
    assert_eq!(
        std::fs::read_to_string(staging.join("active.md")).unwrap(),
        "in progress"
    );
    let result = agent.call("mem_snapshot_restore", json!({"id": id})).await;
    assert!(
        result.contains("another snapshot restore or reclaim is in progress"),
        "{result}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("doc.md")).unwrap(),
        "live"
    );
    assert_eq!(
        std::fs::read_to_string(staging.join("active.md")).unwrap(),
        "in progress"
    );
    drop(lock);
    assert_eq!(
        agent.call("mem_snapshot_restore", json!({"id": id})).await,
        format!("restored {id}")
    );
    assert_eq!(
        std::fs::read_to_string(root.join("doc.md")).unwrap(),
        "snapshot"
    );
    agent.cleanup().await;
    other.cleanup().await;
}
