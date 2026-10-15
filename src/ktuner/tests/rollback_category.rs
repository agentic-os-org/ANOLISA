//! Contract for `ktuner rollback --category`, driving the real binary: the
//! restore and the preview select the entries a rule category recorded, an
//! entry without a category is reported instead of guessed, and the
//! invocations without the flag answer the same bytes as before it existed.
//! The selecting forms need a ledger, hence root, and run against private
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

const MUTUAL_EXCLUSION: &str = "rollback takes either parameters or --category, not both";
const UNKNOWN_CATEGORY: &str = "未知分类: garbage（支持: net, mem, io, cpu, security）";

/// A private fixture: `/var/lib` and `/etc` are the scratch directory's, so
/// the restores only touch these files. The ledger holds a classified entry
/// per category — including a security finding on a `net.*` knob — the
/// recorded mutually exclusive pair, and one entry with no category at all
/// (as a ledger written before the field existed, or a library import, would
/// have). The live `net.core.somaxconn` and `net.core.bpf_jit_harden` are
/// drifted; `/etc/sysctl.d` exists because a restore that leaves entries
/// behind regenerates the persisted file there.
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
    let somaxconn = scratch.0.join("somaxconn");
    let harden = scratch.0.join("bpf_jit_harden");
    let dmesg = scratch.0.join("dmesg_restrict");
    let bytes = scratch.0.join("dirty_bytes");
    let ratio = scratch.0.join("dirty_ratio");
    let swappiness = scratch.0.join("swappiness");
    let legacy = scratch.0.join("legacy_knob");
    fs::write(&somaxconn, "1024").expect("live value");
    fs::write(&harden, "0").expect("live value");
    fs::write(&dmesg, "1").expect("live value");
    fs::write(&bytes, "1073741824").expect("live value");
    fs::write(&ratio, "0").expect("live value");
    fs::write(&swappiness, "1").expect("live value");
    fs::write(&legacy, "7").expect("live value");
    fs::write(
        scratch.0.join("varlib/ktuner/rollback.json"),
        serde_json::json!({"version": 1, "entries": {
            "kernel.dmesg_restrict": {"previous": "0", "applied": "1", "path": dmesg, "category": "security"},
            "net.core.bpf_jit_harden": {"previous": "0", "applied": "1", "path": harden, "category": "security"},
            "net.core.legacy_knob": {"previous": "9", "applied": "7", "path": legacy},
            "net.core.somaxconn": {"previous": "128", "applied": "4096", "path": somaxconn, "category": "performance"},
            "vm.dirty_bytes": {"previous": "0", "applied": "1073741824", "path": bytes, "category": "performance"},
            "vm.dirty_ratio": {"previous": "20", "applied": "0", "path": ratio, "category": "performance"},
            "vm.swappiness": {"previous": "60", "applied": "1", "path": swappiness, "category": "performance"},
        }})
        .to_string(),
    )
    .expect("ledger");
    fs::write(
        scratch.0.join("etc/sysctl.d/99-ktuner.conf"),
        "kernel.dmesg_restrict = 1\nnet.core.bpf_jit_harden = 1\nnet.core.legacy_knob = 7\n\
         net.core.somaxconn = 4096\nvm.dirty_bytes = 1073741824\nvm.swappiness = 1\n",
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
            "rollback-category-test",
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

fn body_of(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).expect("stdout is JSON")
}

#[test]
fn rollback_category_misuse_is_a_usage_error() {
    // Selecting the recorded parameters and selecting the entries of a
    // category are opposite instructions, so carrying both is an argument
    // error: it fires before the root gate (this unprivileged run proves the
    // order) and leaves stdout empty.
    for arguments in [
        &["rollback", "vm.swappiness", "--category", "net"][..],
        &["rollback", "--list", "vm.swappiness", "--category", "net"][..],
        &["rollback", "--category", "net", "vm.swappiness"][..],
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

    // The pre-existing errors keep their bytes and their place: `--drifted`
    // without `--list` answers first, and the `--exclude` misuse outranks the
    // new one when both are present.
    let out = ktuner(&["rollback", "--drifted", "--category", "net"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        error_body("rollback --drifted requires --list")
    );
    let out = ktuner(&[
        "rollback",
        "vm.swappiness",
        "--exclude",
        "vm.dirty_ratio",
        "--category",
        "net",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        error_body("rollback takes either parameters or --exclude, not both")
    );

    // An unknown category is refused before the root gate too, with the same
    // message `check` and `tune` answer.
    for arguments in [
        &["rollback", "--category", "garbage"][..],
        &["rollback", "--list", "--category", "garbage"][..],
        &["rollback", "--category", "net", "--category", "garbage"][..],
    ] {
        let out = ktuner(arguments);
        assert_eq!(out.status.code(), Some(2), "{arguments:?}");
        assert!(out.stdout.is_empty(), "{arguments:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            error_body(UNKNOWN_CATEGORY),
            "{arguments:?}"
        );
    }
}

#[test]
fn rollback_category_answers_the_root_refusal_like_every_other_form() {
    // Regression half of the contract: a valid category changes nothing
    // about the root gate, and every form — with and without the new flag —
    // answers the same root-refusal bytes a caller got before it existed.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    for arguments in [
        &["rollback"][..],
        &["rollback", "--list"][..],
        &["rollback", "--list", "vm.swappiness"][..],
        &["rollback", "vm.swappiness"][..],
        &["rollback", "--exclude", "vm.swappiness"][..],
        &["rollback", "--category", "net"][..],
        &["rollback", "--category", "net", "--category", "mem"][..],
        &[
            "rollback",
            "--category",
            "net",
            "--exclude",
            "vm.swappiness",
        ][..],
        &["rollback", "--list", "--category", "net"][..],
        &["rollback", "--list", "--category", "net", "--drifted"][..],
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
fn rollback_category_restores_only_the_recorded_categories() {
    assert_eq!(unsafe { libc::geteuid() }, 0, "requires root");
    let scratch = scratch_fixture("category-restores");
    let out = isolated_run(&scratch, &["rollback", "--category", "net"]);
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
    assert_eq!(
        body_of(&out),
        serde_json::json!({
            "failed": 0,
            "restored": 1,
            "restored_params": ["net.core.somaxconn"],
            "skipped": 0,
            "status": "Full",
            "unclassified": ["net.core.legacy_knob"],
        }),
        "the counters, the entries it restored, the unclassified report, and nothing besides them"
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("somaxconn")).unwrap(),
        "128",
        "the network entry is restored"
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("bpf_jit_harden")).unwrap(),
        "0",
        "the security finding on a net.* knob has its own category and stays"
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("dmesg_restrict")).unwrap(),
        "1"
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("legacy_knob")).unwrap(),
        "7"
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("swappiness")).unwrap(),
        "1"
    );
    assert_eq!(
        ledger_params(&scratch),
        [
            "kernel.dmesg_restrict",
            "net.core.bpf_jit_harden",
            "net.core.legacy_knob",
            "vm.dirty_bytes",
            "vm.dirty_ratio",
            "vm.swappiness",
        ],
        "every entry the categories did not select stays recorded"
    );
    let persisted = fs::read_to_string(scratch.0.join("etc/sysctl.d/99-ktuner.conf")).unwrap();
    assert!(!persisted.contains("net.core.somaxconn"), "{persisted}");
    assert!(persisted.contains("vm.swappiness = 1"), "{persisted}");
}

#[test]
#[ignore = "requires root and mount namespaces; writes only isolated fixture files"]
fn rollback_category_selects_the_union_the_plan_would() {
    assert_eq!(unsafe { libc::geteuid() }, 0, "requires root");
    let scratch = scratch_fixture("category-union");
    let ledger = scratch.0.join("varlib/ktuner/rollback.json");
    let ledger_before = fs::read(&ledger).unwrap();

    // Several --category values keep the union, and the listing is exactly
    // the set the restore would retire: the network entry, the memory
    // entries and the recorded pair, but not the security finding on the
    // net.* knob.
    let out = isolated_run(
        &scratch,
        &[
            "rollback",
            "--list",
            "--category",
            "net",
            "--category",
            "mem",
        ],
    );
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stderr.is_empty());
    let body = body_of(&out);
    let params: Vec<&str> = body["pending"]
        .as_array()
        .expect("pending is an array")
        .iter()
        .map(|entry| entry["param"].as_str().expect("param"))
        .collect();
    assert_eq!(
        params,
        [
            "net.core.somaxconn",
            "vm.dirty_bytes",
            "vm.dirty_ratio",
            "vm.swappiness"
        ],
        "{body}"
    );
    assert_eq!(body["count"], 4, "{body}");
    assert_eq!(
        body["unclassified"],
        serde_json::json!(["net.core.legacy_knob"]),
        "{body}"
    );
    assert_eq!(
        fs::read(&ledger).unwrap(),
        ledger_before,
        "the preview is read-only"
    );

    // `--drifted` narrows what the selection leaves.
    let out = isolated_run(
        &scratch,
        &[
            "rollback",
            "--list",
            "--category",
            "net",
            "--category",
            "mem",
            "--drifted",
        ],
    );
    assert_eq!(out.status.code(), Some(0));
    let drifted = body_of(&out);
    assert_eq!(drifted["count"], 1, "{drifted}");
    assert_eq!(drifted["pending"][0]["param"], "net.core.somaxconn");

    // The aliases name the same categories, so they answer the same listing.
    let out = isolated_run(
        &scratch,
        &[
            "rollback",
            "--list",
            "--category",
            "network",
            "--category",
            "内存",
        ],
    );
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(body_of(&out), body, "aliases select the same entries");

    // The restore retires that same set and answers the counters of a full
    // rollback plus the unclassified report.
    let out = isolated_run(
        &scratch,
        &["rollback", "--category", "net", "--category", "mem"],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(
        body_of(&out),
        serde_json::json!({
            "failed": 0,
            "restored": 4,
            "restored_params": ["net.core.somaxconn", "vm.dirty_bytes", "vm.dirty_ratio", "vm.swappiness"],
            "skipped": 0,
            "status": "Full",
            "unclassified": ["net.core.legacy_knob"],
        })
    );
    assert_eq!(
        ledger_params(&scratch),
        [
            "kernel.dmesg_restrict",
            "net.core.bpf_jit_harden",
            "net.core.legacy_knob",
        ]
    );
}

#[test]
#[ignore = "requires root and mount namespaces; writes only isolated fixture files"]
fn rollback_category_stacks_exclusions_and_reports_what_it_left() {
    assert_eq!(unsafe { libc::geteuid() }, 0, "requires root");
    let scratch = scratch_fixture("category-excluding");
    let out = isolated_run(
        &scratch,
        &["rollback", "--category", "net", "--exclude", "vm.typo"],
    );
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        body_of(&out),
        serde_json::json!({
            "failed": 0,
            "restored": 1,
            "restored_params": ["net.core.somaxconn"],
            "skipped": 0,
            "status": "Full",
            "unclassified": ["net.core.legacy_knob"],
            "unmatched_exclude": ["vm.typo"],
        }),
        "both reports, and the categories still select on their own"
    );

    // An exclusion on one half of the recorded pair keeps the whole unit: the
    // kernel zeroes either knob when the other is written, so the selected
    // half is not restored either.
    let scratch = scratch_fixture("category-excluding-twin");
    let out = isolated_run(
        &scratch,
        &[
            "rollback",
            "--category",
            "mem",
            "--exclude",
            "vm.dirty_bytes",
        ],
    );
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        body_of(&out),
        serde_json::json!({
            "failed": 0,
            "restored": 1,
            "restored_params": ["vm.swappiness"],
            "skipped": 0,
            "status": "Full",
            "unclassified": ["net.core.legacy_knob"],
        })
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("dirty_bytes")).unwrap(),
        "1073741824",
        "the excluded half is never written"
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("dirty_ratio")).unwrap(),
        "0",
        "neither is the half that shares its unit"
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("swappiness")).unwrap(),
        "60",
        "the other entry of the category is restored"
    );
}

#[test]
#[ignore = "requires root and mount namespaces; writes only isolated fixture files"]
fn rollback_category_leaving_nothing_selected_changes_nothing() {
    assert_eq!(unsafe { libc::geteuid() }, 0, "requires root");
    let scratch = scratch_fixture("category-empty");
    let ledger = scratch.0.join("varlib/ktuner/rollback.json");
    let persisted = scratch.0.join("etc/sysctl.d/99-ktuner.conf");
    let ledger_before = fs::read(&ledger).unwrap();
    let persisted_before = fs::read(&persisted).unwrap();

    let out = isolated_run(&scratch, &["rollback", "--category", "io"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(out.stderr.is_empty());
    assert_eq!(
        body_of(&out),
        serde_json::json!({
            "failed": 0,
            "restored": 0,
            "skipped": 0,
            "status": "Nothing",
            "unclassified": ["net.core.legacy_knob"],
        }),
        "an empty selection answers the empty ledger's counters"
    );
    assert_eq!(fs::read(&ledger).unwrap(), ledger_before);
    assert_eq!(fs::read(&persisted).unwrap(), persisted_before);
    assert_eq!(
        fs::read_to_string(scratch.0.join("somaxconn")).unwrap(),
        "1024"
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("swappiness")).unwrap(),
        "1"
    );
}

#[test]
#[ignore = "requires root and mount namespaces; writes only isolated fixture files"]
fn rollback_category_absent_ledger_is_an_empty_preview() {
    assert_eq!(unsafe { libc::geteuid() }, 0, "requires root");
    let scratch = scratch_fixture("category-absent");
    let ledger = scratch.0.join("varlib/ktuner/rollback.json");
    fs::remove_file(&ledger).unwrap();
    let lock = scratch.0.join("varlib/ktuner/rollback.json.lock");

    let out = isolated_run(
        &scratch,
        &[
            "rollback",
            "--list",
            "--category",
            "net",
            "--exclude",
            "vm.typo",
        ],
    );
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        body_of(&out),
        serde_json::json!({
            "count": 0,
            "pending": [],
            "unmatched_exclude": ["vm.typo"],
        }),
        "no ledger selects nothing, and every name is unrecorded"
    );
    assert!(!ledger.exists(), "the preview must not create the ledger");
    assert!(!lock.exists(), "the preview must not create the lock file");

    // The restore keeps the command error every other rollback form reports.
    let out = isolated_run(&scratch, &["rollback", "--category", "net"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&out.stderr).expect("stderr JSON");
    assert!(
        error["error"]
            .as_str()
            .expect("error is a string")
            .contains("没有找到 rollback 文件"),
        "{error}"
    );
}
