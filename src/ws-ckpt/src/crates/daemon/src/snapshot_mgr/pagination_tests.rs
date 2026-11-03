//! Cursor traversal regressions use ordinary directories as storage placeholders.
//! Listing reads the registered index; these tests do not invoke btrfs commands.

use super::*;
use chrono::Duration;
use ws_ckpt_common::{DaemonConfig, SnapshotIndex};

type Identity = (DateTime<Utc>, String, String);

struct Fixture {
    root: tempfile::TempDir,
    state: Arc<DaemonState>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(crate::backends::btrfs_loop::BtrfsLoopBackend::new(
            root.path().join("data"),
            root.path().join("unused.img"),
        ));
        let state = Arc::new(DaemonState::new(
            DaemonConfig::default(),
            backend,
            root.path().join("state"),
        ));
        Self { root, state }
    }

    fn register(&self, ws_id: &str, snapshots: Vec<(String, SnapshotMeta, bool)>) {
        let live = self.state.backend.data_root().join(ws_id);
        std::fs::create_dir_all(&live).unwrap();
        let path = self.root.path().join(ws_id);
        std::os::unix::fs::symlink(live, &path).unwrap();
        let mut index = SnapshotIndex::new(path.clone());
        for (id, meta, orphan) in snapshots {
            if orphan {
                index.recovered_orphans.insert(id.clone());
            }
            index.snapshots.insert(id, meta);
        }
        self.state
            .register_workspace(ws_id.into(), path, index)
            .unwrap();
    }

    async fn expected(&self, scope: Option<&str>, orphans: bool) -> Vec<Identity> {
        let mut expected = Vec::new();
        for arc in self.state.all_workspaces() {
            let ws = arc.read().await;
            if scope.is_some_and(|id| id != ws.ws_id) {
                continue;
            }
            for (id, meta) in &ws.index.snapshots {
                if !orphans || ws.index.recovered_orphans.contains(id) {
                    expected.push((meta.created_at, ws.ws_id.clone(), id.clone()));
                }
            }
        }
        expected.sort();
        expected
    }

    async fn walk(
        &self,
        scope: Option<&str>,
        limit: usize,
        orphans: bool,
        mut cursor: Option<String>,
    ) -> Vec<Identity> {
        let mut found = Vec::new();
        // A monotonic traversal must terminate, including when entries disappear.
        for _ in 0..1000 {
            let response =
                list_snapshot_page(&self.state, scope, limit, cursor.as_deref(), orphans)
                    .await
                    .unwrap();
            let Response::ListPageOk {
                snapshots,
                next_cursor,
            } = response
            else {
                panic!("expected a snapshot page: {response:?}");
            };
            assert!(snapshots.len() <= limit);
            found.extend(snapshots.iter().map(identity));
            if next_cursor.is_none() {
                assert!(found.windows(2).all(|pair| pair[0] < pair[1]));
                return found;
            }
            assert_ne!(next_cursor, cursor, "a continuation must advance");
            cursor = next_cursor;
        }
        panic!("cursor traversal did not terminate");
    }
}

fn meta(created_at: DateTime<Utc>) -> SnapshotMeta {
    SnapshotMeta {
        created_at,
        message: Some("snapshot".into()),
        metadata: None,
        pinned: false,
        missing: false,
        parent_id: None,
        child_ids: Vec::new(),
    }
}

fn identity(item: &SnapshotListItem) -> Identity {
    let (created_at, workspace, id) = match item {
        SnapshotListItem::Full(entry) => (entry.meta.created_at, &entry.workspace, &entry.id),
        SnapshotListItem::Summary(entry) => (entry.created_at, &entry.workspace, &entry.id),
    };
    (
        created_at,
        Path::new(workspace)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .into(),
        id.clone(),
    )
}

#[tokio::test]
async fn complete_walk_matches_sorted_index_with_ties_scopes_and_orphans() {
    for all_same_timestamp in [false, true] {
        let fx = Fixture::new();
        let future = Utc::now() + Duration::days(365);
        // Registration order and insertion order deliberately differ from list order.
        for ws_id in ["ws-c", "ws-a", "ws-b"] {
            fx.register(
                ws_id,
                (0..53)
                    .rev()
                    .map(|i| {
                        let offset = if all_same_timestamp { 0 } else { i % 5 };
                        (
                            format!("snapshot-{i:03}"),
                            meta(future + Duration::seconds(offset)),
                            i % 7 == 0,
                        )
                    })
                    .collect(),
            );
        }
        fx.register("ws-empty", vec![]);
        for scope in [None, Some("ws-a"), Some("ws-b"), Some("ws-empty")] {
            for orphans in [false, true] {
                let expected = fx.expected(scope, orphans).await;
                for limit in [1, 17, 1000] {
                    assert_eq!(
                        fx.walk(scope, limit, orphans, None).await,
                        expected,
                        "scope={scope:?}, limit={limit}, orphans={orphans}, tied={all_same_timestamp}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn continuation_observes_live_changes_within_its_original_bounds() {
    let fx = Fixture::new();
    let future = Utc::now() + Duration::days(365);
    for ws_id in ["ws-a", "ws-b"] {
        fx.register(
            ws_id,
            (0..5)
                .map(|i| (format!("s{i}"), meta(future + Duration::seconds(i)), false))
                .collect(),
        );
    }
    let first = list_snapshot_page(&fx.state, None, 3, None, false)
        .await
        .unwrap();
    let Response::ListPageOk {
        snapshots,
        next_cursor: Some(cursor),
    } = first
    else {
        panic!("expected a first page with continuation");
    };
    assert_eq!(snapshots.len(), 3);
    let after = identity(snapshots.last().unwrap());
    let upper = fx.expected(None, false).await.pop().unwrap();

    let a = fx.state.get_by_wsid("ws-a").unwrap();
    {
        let mut ws = a.write().await;
        ws.insert_snapshot("s2".into(), meta(future + Duration::seconds(10)));
        ws.insert_snapshot(
            "inserted-in-gap".into(),
            meta(future + Duration::seconds(1) + Duration::nanoseconds(1)),
        );
        ws.insert_snapshot("before-cursor".into(), meta(future));
    }
    let b = fx.state.get_by_wsid("ws-b").unwrap();
    {
        let mut ws = b.write().await;
        ws.remove_snapshot("s1");
        ws.remove_snapshot("s4"); // The initial upper-bound item itself may disappear.
        ws.insert_snapshot("r4".into(), meta(future + Duration::seconds(4)));
        ws.insert_snapshot("z4".into(), meta(future + Duration::seconds(4)));
    }
    fx.register(
        "ws-new",
        vec![(
            "new-workspace".into(),
            meta(future + Duration::seconds(2)),
            false,
        )],
    );
    let expected = fx
        .expected(None, false)
        .await
        .into_iter()
        .filter(|key| key > &after && key <= &upper)
        .collect::<Vec<_>>();
    for limit in [1, 3, 1000] {
        assert_eq!(
            fx.walk(None, limit, false, Some(cursor.clone())).await,
            expected
        );
    }
}

#[tokio::test]
async fn replacing_an_index_updates_same_count_membership_timestamps_and_orphans() {
    let fx = Fixture::new();
    let now = Utc::now();
    fx.register(
        "ws-a",
        vec![
            ("a".into(), meta(now), false),
            ("b".into(), meta(now + Duration::seconds(1)), true),
            ("c".into(), meta(now + Duration::seconds(2)), false),
        ],
    );
    let arc = fx.state.get_by_wsid("ws-a").unwrap();
    {
        let mut ws = arc.write().await;
        let mut replacement = ws.index.clone();
        replacement.snapshots.remove("a");
        replacement.snapshots.get_mut("b").unwrap().created_at = now + Duration::seconds(5);
        replacement.snapshots.insert("d".into(), meta(now));
        replacement.recovered_orphans.clear();
        replacement.recovered_orphans.insert("d".into());
        ws.replace_index(replacement);
    }
    for scope in [None, Some("ws-a")] {
        for orphans in [false, true] {
            assert_eq!(
                fx.walk(scope, 1, orphans, None).await,
                fx.expected(scope, orphans).await
            );
        }
    }
}
