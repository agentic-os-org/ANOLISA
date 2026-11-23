//! Crash discovery before commit and verify complete rollback after SIGKILL.
//! Keep this as a separate test binary: spawning can inherit other tests' lease
//! FDs until exec, delaying lock release after those tests drop their repositories.

use asc_pap::PapRepository;
use asc_policy_repository_sqlite::SqlitePolicyRepository;
use asc_policy_types::scope::{PreparedScope, ScopeSelector, ScopeStatus};
use rusqlite::Transaction;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn before_commit(transaction: &Transaction<'_>) {
    use std::io::Write as _;
    let expected: i64 = std::env::var("ASC_UNCOMMITTED_INSTANCES")
        .unwrap()
        .parse()
        .unwrap();
    // Require the complete aggregate to be staged in this transaction before killing it.
    let bindings: i64 = transaction
        .query_row("SELECT count(*) FROM bindings", [], |row| row.get(0))
        .unwrap();
    let pins: i64 = transaction
        .query_row(
            "SELECT count(*) FROM scopes WHERE pinned_process_json IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(bindings, expected);
    assert_eq!(pins, i64::from(expected == 1));
    // libtest may print a test-name prefix without a trailing newline.
    println!("\nGATE:discovery_before_commit");
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::park();
    }
}

#[test]
fn uncommitted_discovery_child() {
    let Ok(path) = std::env::var("ASC_UNCOMMITTED_DATABASE") else {
        // Ordinary suite runs also discover this subprocess entry point.
        return;
    };
    let binding: asc_policy_types::binding::PreparedBinding = serde_json::from_str(include_str!(
        "../../asc-policy-types/tests/fixtures/prepared-binding.json"
    ))
    .unwrap();
    let count: u32 = std::env::var("ASC_UNCOMMITTED_INSTANCES")
        .unwrap()
        .parse()
        .unwrap();
    let instances: Vec<_> = (0..count)
        .map(|offset| {
            let mut process = binding.scope.process.clone();
            process.pid += offset;
            process
        })
        .collect();
    let repo = SqlitePolicyRepository::open(Path::new(&path)).unwrap();
    SqlitePolicyRepository::before_next_commit_for_test(before_commit);
    repo.sync_scope_instances(&binding.scope.scope_id, &instances)
        .unwrap();
    panic!("discovery committed before the crash gate");
}

#[test]
fn sigkill_before_discovery_commit_rolls_back_pin_and_all_bindings() {
    use std::io::{BufRead as _, BufReader};
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    for count in [1, 200] {
        let directory = directory();
        let path = directory.path().join("policy-state.db");
        let repo = SqlitePolicyRepository::open(&path).unwrap();
        let binding: asc_policy_types::binding::PreparedBinding = serde_json::from_str(
            include_str!("../../asc-policy-types/tests/fixtures/prepared-binding.json"),
        )
        .unwrap();
        repo.put_policy(&binding.policy).unwrap();
        let scope = PreparedScope {
            scope_id: binding.scope.scope_id,
            selector: if count == 1 {
                binding.scope.selector
            } else {
                ScopeSelector::Process {
                    matcher: asc_policy_types::scope::ProcessMatcher::Name {
                        process_name: "worker".into(),
                    },
                }
            },
            policy_snapshots: vec![binding.policy],
            owner_uid: None,
            status: ScopeStatus::Active,
        };
        repo.put_scope(&scope).unwrap();
        let before = repo.scope_discovery_seed(&scope.scope_id).unwrap();
        drop(repo);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "uncommitted_discovery_child", "--nocapture"])
            .env("ASC_UNCOMMITTED_DATABASE", &path)
            .env("ASC_UNCOMMITTED_INSTANCES", count.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let line = line.unwrap();
                if line == "GATE:discovery_before_commit" {
                    sender.send(()).unwrap();
                }
            }
        });
        let gate = receiver.recv_timeout(Duration::from_secs(10));
        let _ = child.kill();
        let status = child.wait().unwrap();
        reader.join().unwrap();
        gate.expect("child did not reach the uncommitted production transaction");
        assert_eq!(status.signal(), Some(9));
        let repo = SqlitePolicyRepository::open(&path).unwrap();
        let after = repo.scope_discovery_seed(&scope.scope_id).unwrap();
        assert_eq!(after.scope, before.scope);
        assert_eq!(after.pinned_process, before.pinned_process);
        assert!(after.instances.is_empty());
        assert_eq!(repo.list_bindings(1000, 0).unwrap().total, 0);
        let instances: Vec<_> = (0..count)
            .map(|offset| {
                let mut process = binding.scope.process.clone();
                process.pid += offset;
                process
            })
            .collect();
        assert_eq!(
            repo.sync_scope_instances(&scope.scope_id, &instances)
                .unwrap()
                .len(),
            count as usize
        );
    }
}

fn directory() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}
