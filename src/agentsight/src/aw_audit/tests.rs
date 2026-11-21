//! Exercise cursor and object atomicity using the real SQLite store and CLI subprocess.

use super::*;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    time::{Duration, Instant},
};

fn anchor(sequence: u64) -> Value {
    json!({"sequence": sequence, "digest": format!("{sequence:064x}")})
}

fn response(source: &str, result: Value, error: Option<&str>) -> Value {
    json!({"api_version":"audit-export.aw/v1alpha1","source_id":source,"result":result,"error":error})
}

fn cursor(source: &str, chain: &str, sequence: u64) -> Value {
    json!({
        "source_id":source,"chain_id":chain,"consumed":anchor(sequence),"observed_tip":anchor(sequence),
    })
}

fn page(source: &str, chain: &str, sequence: u64, terminal: bool, change: bool) -> Value {
    let producer = json!({"producerId":"aw-service","scopeId":source,"generation":"generation"});
    let subject =
        json!({"instanceId":"instance","sessionId":"native-session","toolCallId":"native-call"});
    let event = json!({"apiVersion":"runtime.aw/v1alpha1","kind":"ExecutionEvent",
        "metadata":{"uid":chain,"producer":producer,"createdAt":"2026-01-01T00:00:00Z"},
        "data":{"subject":subject,"name":"tool.before","details":{"change":change}}});
    let record = json!({"apiVersion":"audit.aw/v1alpha1","kind":"AuditRecord",
        "metadata":{"uid":format!("{chain_prefix}{sequence:02x}", chain_prefix=&chain[..62]),
            "producer":producer,"createdAt":"2026-01-01T00:00:01Z"},
        "data":{"chain":{"id":chain,"sequence":sequence},"subject":subject,
            "eventRef":{"uid":chain,"producer":producer}}});
    response(
        source,
        json!({"type":"read","event":event,"records":[record],"chain_id":chain,
            "cursor":cursor(source,chain,sequence),"tip":anchor(sequence),"caught_up":true,"terminal":terminal}),
        None,
    )
}

struct Fixture(tempfile::TempDir);

impl Fixture {
    fn new() -> Self {
        let fixture = Self(tempfile::tempdir().unwrap());
        let binary = fixture.0.path().join("aw");
        fs::write(
            &binary,
            r#"#!/usr/bin/env python3
import json
import pathlib
import sys
assert sys.argv[1] == "audit-export"
root = pathlib.Path(sys.argv[3])
request = json.loads(pathlib.Path(sys.argv[5]).read_text())["operation"]
fixture = json.loads((root / "response.json").read_text())
entry = fixture["enumerate"]
if request["type"] == "read":
    chain = request["chain_id"]
    with (root / "reads").open("a") as reads:
        print(chain, file=reads)
    item = fixture["read"].get(chain)
    if item is None:
        entry = dict(entry, result=None, error="history_missing")
    else:
        assert request["cursor"] == item["expected_cursor"], request
        entry = item["response"]
print(json.dumps(entry))
sys.exit(int(entry["error"] is not None))
"#,
        )
        .unwrap();
        fs::set_permissions(binary, fs::Permissions::from_mode(0o700)).unwrap();
        fixture
    }

    fn export(&self) -> Export {
        Export::new(self.0.path().join("aw"), self.0.path().into()).unwrap()
    }

    fn consumer(&self) -> Consumer {
        Consumer::open(self.export(), self.0.path()).unwrap()
    }

    fn save(&self, source: &str, chains: &[(&str, Value, Value)]) {
        let keys: Vec<_> = chains.iter().map(|(chain, _, _)| chain).collect();
        let read: serde_json::Map<_, _> = chains
            .iter()
            .map(|(chain, cursor, response)| {
                (
                    chain.to_string(),
                    json!({"expected_cursor":cursor,"response":response}),
                )
            })
            .collect();
        fs::write(
            self.0.path().join("response.json"),
            json!({
                "enumerate":response(source,json!({"type":"enumerate","keys":keys,"next_cursor":null}),None),
                "read":read,
            })
            .to_string(),
        )
        .unwrap();
    }

    fn timeline(&self, source: &str) -> Value {
        query(self.0.path(), source, Some("instance"), 0, i64::MAX).unwrap()
    }

    fn wait_for_state(&self, source: &str, state: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let timeline = self.timeline(source);
            if timeline["ingestion"]["state"] == state {
                return timeline;
            }
            assert!(
                Instant::now() < deadline,
                "expected {state}, got {timeline}"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }
}

#[test]
fn restart_resumes_open_chain_and_conflicts_roll_back_the_cursor() {
    let fixture = Fixture::new();
    let first = "a".repeat(64);
    let second = "b".repeat(64);
    let id = "c".repeat(64);
    let running = AtomicBool::new(true);
    let fixture_page = |source: &str, terminal, change| {
        let (previous, sequence) = if source == first && terminal {
            (cursor(source, &id, 0), 1)
        } else {
            (Value::Null, 0)
        };
        fixture.save(
            source,
            &[(&id, previous, page(source, &id, sequence, terminal, change))],
        );
    };

    fixture_page(&first, false, false);
    fixture.consumer().poll(&running).unwrap();
    assert_eq!(fixture.timeline(&first)["pending"], 1);
    assert_eq!(
        fixture.timeline(&first)["objects"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let mut consumer = fixture.consumer();
    fixture.save(&first, &[]);
    consumer.poll(&running).unwrap();
    assert_eq!(
        fixture.timeline(&first)["recent_gaps"][0]["code"],
        "history_missing"
    );

    fixture_page(&first, true, true);
    consumer.poll(&running).unwrap();
    assert_eq!(fixture.timeline(&first)["pending"], 1);
    assert_eq!(
        fixture.timeline(&first)["objects"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        fixture.timeline(&first)["recent_gaps"][0]["code"],
        "integrity_or_storage_error"
    );

    drop(consumer);
    fixture_page(&first, true, false);
    let mut consumer = fixture.consumer();
    consumer.poll(&running).unwrap();
    assert_eq!(fixture.timeline(&first)["pending"], 0);
    assert_eq!(fixture.timeline(&first)["gaps"], 0);
    assert_eq!(
        fixture.timeline(&first)["objects"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

    fixture_page(&second, true, true);
    consumer.poll(&running).unwrap();
    assert_eq!(
        fixture.timeline(&first)["ingestion"]["state"],
        "source_changed"
    );
    assert_eq!(
        fixture.timeline(&first)["objects"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        fixture.timeline(&second)["objects"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn restart_skips_retired_terminal_chains_and_polls_an_open_chain() {
    let fixture = Fixture::new();
    let source = "a".repeat(64);
    let closed = ['b', 'c', 'd', 'e'].map(|letter| letter.to_string().repeat(64));
    let open = "f".repeat(64);
    let mut pages: Vec<_> = closed
        .iter()
        .map(|chain| {
            (
                chain.as_str(),
                Value::Null,
                page(&source, chain, 0, true, false),
            )
        })
        .collect();
    pages.push((&open, Value::Null, page(&source, &open, 0, false, false)));
    fixture.save(&source, &pages);
    let running = AtomicBool::new(true);
    let mut consumer = fixture.consumer();
    consumer.poll(&running).unwrap();
    consumer.poll(&running).unwrap();
    assert_eq!(fixture.timeline(&source)["pending"], 1);
    assert_eq!(
        fixture.timeline(&source)["objects"]
            .as_array()
            .unwrap()
            .len(),
        10
    );
    drop(consumer);

    let reads = fixture.0.path().join("reads");
    fs::remove_file(&reads).unwrap();
    fixture.save(
        &source,
        &[(
            &open,
            cursor(&source, &open, 0),
            page(&source, &open, 1, false, false),
        )],
    );
    fixture.consumer().poll(&running).unwrap();
    assert_eq!(
        fs::read_to_string(reads)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [open.as_str()]
    );
    let timeline = fixture.timeline(&source);
    assert_eq!(timeline["chains"], 5);
    assert_eq!(timeline["pending"], 1);
    assert_eq!(timeline["gaps"], 0);
    assert_eq!(timeline["objects"].as_array().unwrap().len(), 11);
}

#[test]
fn offline_restart_reports_unavailable_and_preserves_last_success() {
    let fixture = Fixture::new();
    let source = "a".repeat(64);
    let chain = "b".repeat(64);
    fixture.save(
        &source,
        &[(&chain, Value::Null, page(&source, &chain, 0, true, false))],
    );
    let worker = Worker::start(fixture.export(), fixture.0.path()).unwrap();
    let online = fixture.wait_for_state(&source, "reachable");
    drop(worker);
    let last_success = &online["ingestion"]["last_success_at_ms"];
    assert!(last_success.is_i64());
    assert_eq!(last_success, &online["ingestion"]["checked_at_ms"]);

    fs::remove_file(fixture.0.path().join("response.json")).unwrap();
    let offline = fixture.consumer();
    let starting = fixture.timeline(&source);
    assert_eq!(starting["ingestion"]["state"], "unknown");
    assert_eq!(&starting["ingestion"]["last_success_at_ms"], last_success);
    drop(offline);

    let worker = Worker::start(fixture.export(), fixture.0.path()).unwrap();
    let offline = fixture.wait_for_state(&source, "unavailable");
    drop(worker);
    assert_eq!(&offline["ingestion"]["last_success_at_ms"], last_success);
    assert_eq!(offline["objects"], online["objects"]);
}

#[test]
fn legacy_database_excludes_completed_chains_from_ingestion_gaps() {
    let root = tempfile::tempdir().unwrap();
    let source = "a".repeat(64);
    let closed = "b".repeat(64);
    let pending = "c".repeat(64);
    let db =
        crate::private_sqlite::open_private_connection(&root.path().join("aw-audit"), "audit.db")
            .unwrap();
    db.execute_batch(
        "CREATE TABLE aw_chains (
            source TEXT NOT NULL, chain TEXT NOT NULL, cursor TEXT, done INTEGER NOT NULL DEFAULT 0,
            checked_at INTEGER NOT NULL DEFAULT 0, gap TEXT, PRIMARY KEY (source, chain)
         );
         CREATE TABLE aw_ingestion (source TEXT PRIMARY KEY, checked_at INTEGER NOT NULL, state TEXT NOT NULL);
         CREATE INDEX aw_chains_schedule ON aw_chains(source, checked_at, chain);",
    )
    .unwrap();
    db.execute(
        "INSERT INTO aw_ingestion VALUES (?1, 100, 'reachable')",
        [&source],
    )
    .unwrap();
    let checkpoint = cursor(&source, &closed, 1).to_string();
    db.execute(
        "INSERT INTO aw_chains VALUES
            (?1, ?2, ?3, 1, 300, 'history_missing'),
            (?1, ?4, NULL, 0, 200, 'history_missing')",
        rusqlite::params![source, closed, checkpoint, pending],
    )
    .unwrap();
    drop(db);

    let store = Storage::open_aw_audit(root.path()).unwrap();
    let scheduled = store.next_chains(&source).unwrap();
    assert_eq!(scheduled.len(), 1);
    assert_eq!(scheduled[0].id, pending);
    let timeline = query(root.path(), &source, None, 0, i64::MAX).unwrap();
    assert_eq!(timeline["chains"], 2);
    assert_eq!(timeline["pending"], 1);
    assert_eq!(timeline["gaps"], 1);
    assert_eq!(
        timeline["recent_gaps"],
        json!([{"chain_id": pending, "code": "history_missing"}])
    );
    assert_eq!(timeline["ingestion"]["state"], "unknown");
    assert_eq!(timeline["ingestion"]["last_success_at_ms"], 100);
}

#[test]
fn symlinked_storage_root_imports_and_queries_while_rejecting_symlinked_state() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let real_base = fixture.0.path().join("real-storage");
    fs::create_dir(&real_base).unwrap();
    let alias_base = fixture.0.path().join("storage-alias");
    symlink(&real_base, &alias_base).unwrap();

    let source = "a".repeat(64);
    let chain = "b".repeat(64);
    fixture.save(
        &source,
        &[(&chain, Value::Null, page(&source, &chain, 0, true, false))],
    );

    let worker = Worker::start(fixture.export(), &alias_base).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let via_alias = loop {
        if let Ok(value) = query(&alias_base, &source, None, 0, i64::MAX) {
            if value["ingestion"]["state"] == "reachable"
                && value["objects"].as_array().is_some_and(|o| !o.is_empty())
            {
                break value;
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for import through symlinked storage root"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    drop(worker);

    let via_real = query(&real_base, &source, None, 0, i64::MAX).unwrap();
    assert_eq!(via_alias["objects"], via_real["objects"]);
    assert_eq!(via_alias["objects"].as_array().unwrap().len(), 2);

    // Symlinked sidecar inside aw-audit must still be rejected.
    let decoy = fixture.0.path().join("decoy");
    fs::write(&decoy, []).unwrap();
    fs::set_permissions(&decoy, fs::Permissions::from_mode(0o600)).unwrap();
    let journal = real_base.join("aw-audit").join("audit.db-journal");
    symlink(&decoy, &journal).unwrap();
    assert!(query(&alias_base, &source, None, 0, i64::MAX).is_err());
    fs::remove_file(&journal).unwrap();
    assert!(query(&alias_base, &source, None, 0, i64::MAX).is_ok());

    // Replacing aw-audit directory itself with a symlink must still be rejected.
    let moved = fixture.0.path().join("moved-aw-audit");
    fs::rename(real_base.join("aw-audit"), &moved).unwrap();
    symlink(&moved, real_base.join("aw-audit")).unwrap();
    assert!(query(&alias_base, &source, None, 0, i64::MAX).is_err());
}
