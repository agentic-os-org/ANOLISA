//! Contract for `ktuner rollback --exclude`, driving the real binary: the
//! restore and the preview narrow an isolated ledger to the entries the names
//! do not address, an inert name is reported instead of refused, and the
//! invocations without the flag answer the same bytes as before it existed.
//! The exclusive forms need a ledger, hence root, and run against private
//! mount-namespace fixtures (ignored by default); the usage errors and the
//! no-flag regression run unprivileged. This file deliberately never runs a
//! rollback that could reach a real ledger.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

fn ktuner(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ktuner"))
        .args(arguments)
        .output()
        .expect("run ktuner")
}

/// The exact bytes the binary emits for an error: the README's stderr JSON,
/// pretty-printed like every other body it prints.
fn error_body(message: &str) -> String {
    format!(
        "{}\n",
        serde_json::to_string_pretty(&serde_json::json!({ "error": message })).unwrap()
    )
}

const MUTUAL_EXCLUSION: &str = "rollback takes either parameters or --exclude, not both";

/// A private fixture: `/var/lib` and `/etc` are the scratch directory's, so
/// the restores only touch these files, and the ledger holds a recorded
/// mutually exclusive pair plus two plain entries. The live
/// `net.core.somaxconn` is drifted (recorded applied 4096, live 1024).
/// `/etc/sysctl.d` exists because a restore that leaves entries behind
/// regenerates the persisted file there.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scratch_fixture(label: &str) -> Scratch {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let scratch = Scratch(
        std::env::temp_dir().join(format!("ktuner-{label}-{}-{nonce}", std::process::id())),
    );
    fs::create_dir_all(scratch.0.join("varlib/ktuner")).expect("ledger directory");
    fs::create_dir(scratch.0.join("etc")).expect("isolated persistence directory");
    fs::create_dir(scratch.0.join("etc/sysctl.d")).expect("isolated sysctl.d directory");
    let swappiness = scratch.0.join("swappiness");
    let somaxconn = scratch.0.join("somaxconn");
    let bytes = scratch.0.join("dirty_bytes");
    let ratio = scratch.0.join("dirty_ratio");
    fs::write(&swappiness, "1").expect("live value");
    fs::write(&somaxconn, "1024").expect("live value");
    fs::write(&bytes, "1073741824").expect("live value");
    fs::write(&ratio, "0").expect("live value");
    fs::write(
        scratch.0.join("varlib/ktuner/rollback.json"),
        serde_json::json!({"version": 1, "entries": {
            "vm.swappiness": {"previous": "60", "applied": "1", "path": swappiness},
            "net.core.somaxconn": {"previous": "128", "applied": "4096", "path": somaxconn},
            "vm.dirty_bytes": {"previous": "0", "applied": "1073741824", "path": bytes},
            "vm.dirty_ratio": {"previous": "20", "applied": "0", "path": ratio},
        }})
        .to_string(),
    )
    .expect("ledger");
    fs::write(
        scratch.0.join("etc/sysctl.d/99-ktuner.conf"),
        "vm.swappiness = 1\nnet.core.somaxconn = 4096\nvm.dirty_bytes = 1073741824\n",
    )
    .expect("persisted file");
    scratch
}

/// Run the real binary against `scratch`'s isolated ledger.
fn isolated_run(scratch: &Scratch, arguments: &[&str]) -> Output {
    Command::new("unshare")
        .args([
            "--mount",
            "--propagation",
            "private",
            "sh",
            "-c",
            "mount --bind \"$1\" /var/lib && mount --bind \"$2\" /etc && \
             bin=\"$3\" && shift 3 && exec \"$bin\" \"$@\"",
            "rollback-exclude-test",
        ])
        .arg(scratch.0.join("varlib"))
        .arg(scratch.0.join("etc"))
        .arg(env!("CARGO_BIN_EXE_ktuner"))
        .args(arguments)
        .output()
        .expect("run isolated ktuner")
}

fn ledger_params(scratch: &Scratch) -> Vec<String> {
    let ledger = scratch.0.join("varlib/ktuner/rollback.json");
    let data: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(ledger).expect("ledger readable")).expect("JSON");
    data["entries"]
        .as_object()
        .expect("entries object")
        .keys()
        .cloned()
        .collect()
}

#[test]
fn rollback_exclude_misuse_is_a_usage_error() {
    // Restoring named parameters and excluding named parameters are opposite
    // instructions, so carrying both is an argument error: it fires before
    // the root gate (this unprivileged run proves the order) and leaves
    // stdout empty.
    for arguments in [
        &["rollback", "vm.swappiness", "--exclude", "vm.dirty_ratio"][..],
        &[
            "rollback",
            "--list",
            "vm.swappiness",
            "--exclude",
            "vm.dirty_ratio",
        ][..],
        &["rollback", "--exclude", "vm.dirty_ratio", "vm.swappiness"][..],
    ] {
        let out = ktuner(arguments);
        assert_eq!(out.status.code(), Some(2), "{arguments:?}");
        assert!(out.stdout.is_empty(), "{arguments:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            error_body(MUTUAL_EXCLUSION),
            "{arguments:?}"
        );
    }

    // The pre-existing `--drifted` usage error keeps its bytes and its place:
    // it answers first when both misuses are present.
    let out = ktuner(&["rollback", "--drifted", "--exclude", "vm.swappiness"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        error_body("rollback --drifted requires --list")
    );
}

#[test]
fn rollback_exclude_answers_the_root_refusal_like_every_other_form() {
    // Regression half of the contract: without a usable ledger every form —
    // with and without the new flag — answers the same root-refusal bytes a
    // caller got before `--exclude` existed.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    for arguments in [
        &["rollback"][..],
        &["rollback", "--list"][..],
        &["rollback", "--list", "vm.swappiness"][..],
        &["rollback", "vm.swappiness"][..],
        &["rollback", "vm.swappiness", "net.core.somaxconn"][..],
        &["rollback", "--exclude", "vm.swappiness"][..],
        &[
            "rollback",
            "--exclude",
            "vm.swappiness",
            "--exclude",
            "net.core.somaxconn",
        ][..],
        &["rollback", "--list", "--exclude", "vm.swappiness"][..],
    ] {
        let out = ktuner(arguments);
        assert_eq!(out.status.code(), Some(2), "{arguments:?}");
        assert!(out.stdout.is_empty(), "{arguments:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            error_body("rollback requires root (sudo ktuner rollback)"),
            "{arguments:?} must answer the same bytes as before the flag existed"
        );
    }
}

#[test]
#[ignore = "requires root and mount namespaces; writes only isolated fixture files"]
fn rollback_exclude_restores_everything_but_the_named_entries() {
    assert_eq!(unsafe { libc::geteuid() }, 0, "requires root");
    let scratch = scratch_fixture("exclude-restores");
    let out = isolated_run(&scratch, &["rollback", "--exclude", "vm.swappiness"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(
        body,
        serde_json::json!({"failed": 0, "restored": 3, "skipped": 0, "status": "Full",
            "restored_params": ["net.core.somaxconn", "vm.dirty_bytes", "vm.dirty_ratio"]}),
        "the counters, the entries it restored, and nothing besides them: {body}"
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("swappiness")).unwrap(),
        "1",
        "the excluded entry stays live"
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("somaxconn")).unwrap(),
        "128"
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("dirty_bytes")).unwrap(),
        "0"
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("dirty_ratio")).unwrap(),
        "20",
        "the pair is restored as one unit"
    );
    assert_eq!(
        ledger_params(&scratch),
        ["vm.swappiness"],
        "the excluded entry is the only record that stays"
    );
    let persisted = fs::read_to_string(scratch.0.join("etc/sysctl.d/99-ktuner.conf")).unwrap();
    assert!(persisted.contains("vm.swappiness = 1"), "{persisted}");
    for gone in ["net.core.somaxconn", "vm.dirty_bytes", "vm.dirty_ratio"] {
        assert!(!persisted.contains(gone), "{gone} must leave {persisted}");
    }
}

#[test]
#[ignore = "requires root and mount namespaces; writes only isolated fixture files"]
fn rollback_exclude_keeps_the_whole_recorded_pair() {
    assert_eq!(unsafe { libc::geteuid() }, 0, "requires root");
    let scratch = scratch_fixture("exclude-pair");
    let out = isolated_run(&scratch, &["rollback", "--exclude", "vm.dirty_bytes"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(
        body,
        serde_json::json!({"failed": 0, "restored": 2, "skipped": 0, "status": "Full",
            "restored_params": ["net.core.somaxconn", "vm.swappiness"]}),
        "{body}"
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("dirty_bytes")).unwrap(),
        "1073741824",
        "the excluded entry is not written"
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("dirty_ratio")).unwrap(),
        "0",
        "neither is the half the ledger recorded with it"
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("swappiness")).unwrap(),
        "60"
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("somaxconn")).unwrap(),
        "128"
    );
    assert_eq!(
        ledger_params(&scratch),
        ["vm.dirty_bytes", "vm.dirty_ratio"],
        "the whole unit stays recorded"
    );
    let persisted = fs::read_to_string(scratch.0.join("etc/sysctl.d/99-ktuner.conf")).unwrap();
    assert!(
        persisted.contains("vm.dirty_bytes = 1073741824"),
        "{persisted}"
    );
    for gone in ["vm.swappiness", "net.core.somaxconn", "vm.dirty_ratio"] {
        assert!(!persisted.contains(gone), "{persisted}");
    }
}

#[test]
#[ignore = "requires root and mount namespaces; writes only isolated fixture files"]
fn rollback_exclude_reports_an_unmatched_name_and_restores_all() {
    assert_eq!(unsafe { libc::geteuid() }, 0, "requires root");
    let scratch = scratch_fixture("exclude-unmatched");
    let out = isolated_run(&scratch, &["rollback", "--exclude", "vm.typo"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(
        body,
        serde_json::json!({
            "failed": 0,
            "restored": 4,
            "restored_params": ["net.core.somaxconn", "vm.dirty_bytes", "vm.dirty_ratio", "vm.swappiness"],
            "skipped": 0,
            "status": "Full",
            "unmatched_exclude": ["vm.typo"],
        }),
        "the inert name is reported and every entry is still restored: {body}"
    );
    assert!(
        !scratch.0.join("varlib/ktuner/rollback.json").exists(),
        "an inert exclusion still restores everything, so the ledger finalizes"
    );
    assert!(!scratch.0.join("etc/sysctl.d/99-ktuner.conf").exists());
}

#[test]
#[ignore = "requires root and mount namespaces; writes only isolated fixture files"]
fn rollback_exclude_with_every_entry_excluded_changes_nothing() {
    assert_eq!(unsafe { libc::geteuid() }, 0, "requires root");
    let scratch = scratch_fixture("exclude-empty");
    let ledger = scratch.0.join("varlib/ktuner/rollback.json");
    let ledger_before = fs::read(&ledger).unwrap();
    let persisted = scratch.0.join("etc/sysctl.d/99-ktuner.conf");
    let persisted_before = fs::read(&persisted).unwrap();

    let out = isolated_run(
        &scratch,
        &[
            "rollback",
            "--exclude",
            "vm.swappiness",
            "--exclude",
            "net.core.somaxconn",
            "--exclude",
            "vm.dirty_bytes",
            "--exclude",
            "vm.dirty_ratio",
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(
        body,
        serde_json::json!({"failed": 0, "restored": 0, "skipped": 0, "status": "Nothing"}),
        "an empty attempt answers the counters an empty ledger answers: {body}"
    );
    assert_eq!(fs::read(&ledger).unwrap(), ledger_before);
    assert_eq!(fs::read(&persisted).unwrap(), persisted_before);
    assert_eq!(
        fs::read_to_string(scratch.0.join("swappiness")).unwrap(),
        "1"
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("somaxconn")).unwrap(),
        "1024"
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("dirty_bytes")).unwrap(),
        "1073741824"
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("dirty_ratio")).unwrap(),
        "0"
    );
}

#[test]
#[ignore = "requires root and mount namespaces; writes only isolated fixture files"]
fn rollback_list_exclude_previews_the_restore_set() {
    assert_eq!(unsafe { libc::geteuid() }, 0, "requires root");
    let scratch = scratch_fixture("exclude-preview");
    let ledger = scratch.0.join("varlib/ktuner/rollback.json");
    let ledger_before = fs::read(&ledger).unwrap();

    let out = isolated_run(
        &scratch,
        &["rollback", "--list", "--exclude", "vm.swappiness"],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(
        body.as_object().unwrap().len(),
        2,
        "the unfiltered listing's keys, nothing added: {body}"
    );
    assert_eq!(body["count"], 3, "{body}");
    let params: Vec<&str> = body["pending"]
        .as_array()
        .expect("pending is an array")
        .iter()
        .map(|entry| entry["param"].as_str().expect("param"))
        .collect();
    assert_eq!(
        params,
        ["net.core.somaxconn", "vm.dirty_bytes", "vm.dirty_ratio"],
        "every recorded entry the name does not address, pair kept whole: {body}"
    );
    assert_eq!(
        fs::read(&ledger).unwrap(),
        ledger_before,
        "the preview is read-only"
    );

    // `--drifted` stacks on what the exclusion leaves.
    let out = isolated_run(
        &scratch,
        &[
            "rollback",
            "--list",
            "--exclude",
            "vm.swappiness",
            "--drifted",
        ],
    );
    assert_eq!(out.status.code(), Some(0));
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(body["count"], 1, "{body}");
    assert_eq!(body["pending"][0]["param"], "net.core.somaxconn");

    // A name the ledger does not record comes back and excludes nothing.
    let out = isolated_run(&scratch, &["rollback", "--list", "--exclude", "vm.typo"]);
    assert_eq!(out.status.code(), Some(0));
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(
        body["unmatched_exclude"],
        serde_json::json!(["vm.typo"]),
        "{body}"
    );
    assert_eq!(body["count"], 4, "{body}");

    // The listing is exactly the set the restore then retires.
    let out = isolated_run(&scratch, &["rollback", "--exclude", "vm.swappiness"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(ledger_params(&scratch), ["vm.swappiness"]);
}
