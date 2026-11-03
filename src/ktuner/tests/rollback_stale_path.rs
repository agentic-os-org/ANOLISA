//! A ledger whose recorded path is an older translation of a live parameter is
//! still restorable, using private procfs fixtures.
#![cfg(target_os = "linux")]

use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// The stale spelling a pre-`5448` ktuner recorded for the dotted interface
/// `Br0.100`: the blanket dot-to-slash translation split the interface name.
const STALE: &str = "/proc/sys/net/ipv4/conf/Br0/100/forwarding";
const LIVE: &str = "/proc/sys/net/ipv4/conf/Br0.100/forwarding";
const KEY: &str = "net.ipv4.conf.Br0.100.forwarding";

fn ktuner(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ktuner"))
        .args(args)
        .output()
        .unwrap()
}

fn body(fixture: &str) {
    let ledger = format!("{fixture}/varlib/ktuner/rollback.json");
    // The record names a parameter that is live at LIVE and gone at STALE.
    let list = ktuner(&["rollback", "--list"]);
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    let listed: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(
        listed["pending"][0]["param"], KEY,
        "the entry is listed: {listed}"
    );
    assert_eq!(
        listed["pending"][0]["live"], "1",
        "the knob is read through the path the name resolves to: {listed}"
    );

    let out = ktuner(&["rollback"]);
    assert!(
        out.status.success(),
        "rollback must restore the live knob: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let outcome: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(outcome["status"], "Full", "{outcome}");
    assert_eq!(outcome["restored"], 1, "{outcome}");
    assert_eq!(
        fs::read_to_string(LIVE).unwrap().trim(),
        "0",
        "the recorded original came back"
    );
    assert!(
        !std::path::Path::new(&ledger).exists(),
        "a complete rollback retires the ledger"
    );
    assert!(
        !std::path::Path::new(STALE).exists(),
        "the stale spelling was never written"
    );
}

#[test]
#[ignore = "requires root and private mount namespaces; only fixture files are written"]
fn rollback_adopts_a_stale_recorded_path() {
    if std::env::var_os("KTUNER_STALE_CHILD").is_some() {
        body(&std::env::var("KTUNER_STALE_FIXTURE").unwrap());
        return;
    }
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ktuner-stale-{}-{nonce}", std::process::id()));
    fs::create_dir_all(dir.join("varlib/ktuner")).unwrap();
    fs::create_dir_all(dir.join("sysctl")).unwrap();
    fs::create_dir_all(dir.join("ktuner-etc")).unwrap();
    fs::create_dir_all(dir.join("systemd")).unwrap();
    fs::create_dir_all(dir.join("ipv4/Br0.100")).unwrap();
    fs::write(dir.join("ipv4/Br0.100/forwarding"), "1").unwrap();
    fs::write(
        dir.join("varlib/ktuner/rollback.json"),
        serde_json::json!({
            "version": 1,
            "entries": { KEY: {"previous": "0", "applied": "1", "path": STALE} },
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.join("sysctl/99-ktuner.conf"),
        "net/ipv4/conf/Br0.100/forwarding = 1\n",
    )
    .unwrap();

    let live_before = fs::read_to_string("/proc/sys/net/ipv4/conf/all/forwarding").unwrap();
    let out = Command::new("unshare")
        .args([
            "--mount",
            "--propagation",
            "private",
            "sh",
            "-ec",
            "mount --bind \"$1/varlib\" /var/lib; \
             mount --bind \"$1/sysctl\" /etc/sysctl.d; \
             mount --bind \"$1/ktuner-etc\" /etc/ktuner; \
             mount --bind \"$1/systemd\" /etc/systemd/system; \
             mount --bind \"$1/ipv4\" /proc/sys/net/ipv4/conf; \
             KTUNER_STALE_CHILD=1 KTUNER_STALE_FIXTURE=\"$1\" exec \"$2\" \
             --exact rollback_adopts_a_stale_recorded_path --ignored --nocapture",
            "stale-test",
        ])
        .arg(&dir)
        .arg(std::env::current_exe().unwrap())
        .output()
        .unwrap();
    assert_eq!(
        fs::read_to_string("/proc/sys/net/ipv4/conf/all/forwarding").unwrap(),
        live_before,
        "the host kept its own forwarding value"
    );
    let _ = fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
