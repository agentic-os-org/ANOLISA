//! The `fix` output's `previous` must be the original the ledger recorded,
//! not the value the diagnosis gathered — against files in a private mount
//! namespace.
#![cfg(target_os = "linux")]

use ktuner_engine::{rules::Recommendation, tuner};
use std::fs;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LEDGER: &str = "/var/lib/ktuner/rollback.json";
const PARAM: &str = "net.core.somaxconn";
const LIVE: &str = "/proc/sys/net/core/somaxconn";

/// A recommendation whose gathered `current_value` is stale: the knob moved
/// between the diagnosis and the apply, which is exactly the window the
/// transaction lock (#5894's under-lock original read) exists for.
fn stale_rec() -> Recommendation {
    Recommendation {
        param: PARAM.into(),
        current_value: "128".into(),
        recommended_value: "65535".into(),
        writable: true,
        ..Default::default()
    }
}

fn body() {
    fs::create_dir_all("/var/lib/ktuner").unwrap();
    // The knob a stale diagnosis last saw as 128 is now 1024.
    fs::write(LIVE, "1024").unwrap();
    let _ = fs::remove_file(LEDGER);

    let fix = tuner::apply_one(&stale_rec()).unwrap();
    // The write itself is unaffected by the staleness.
    assert_eq!(fix.outcome.effective, "65535");
    assert!(!fix.outcome.clamped);

    // The reported original is the one the transaction captured under the
    // lock — the value the ledger records and a rollback restores. The old
    // output printed the gathered rec.current_value (128), contradicting the
    // ledger and `rollback --list` on the very knob it just fixed.
    assert_eq!(
        fix.recorded_previous.as_deref(),
        Some("1024"),
        "fix must report the under-lock original, not the stale gathered value"
    );

    // The ledger agrees: this is what `ktuner rollback` restores.
    let ledger: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(LEDGER).unwrap()).unwrap();
    assert_eq!(ledger["entries"][PARAM]["previous"], "1024");
    assert_eq!(ledger["entries"][PARAM]["applied"], "65535");

    // And the rollback brings the under-lock original back, not the stale one.
    assert!(tuner::rollback_quiet().unwrap().is_complete());
    assert_eq!(fs::read_to_string(LIVE).unwrap(), "1024");
}

#[test]
#[ignore = "requires root and private mount namespaces; only fixture files are written"]
fn fix_reports_the_recorded_previous() {
    if std::env::var_os("KTUNER_FIX_PREVIOUS_CHILD").is_some() {
        body();
        return;
    }
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "ktuner-fix-previous-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(dir.join("varlib")).unwrap();
    fs::create_dir_all(dir.join("etc/sysctl.d")).unwrap();
    fs::write(dir.join("somaxconn"), "1024").unwrap();
    let before = fs::read_to_string(LIVE).unwrap();
    // A short settle so the fixture bind does not race the parent's read.
    std::thread::sleep(Duration::from_millis(50));
    let out = Command::new("unshare").args(["--mount", "--propagation", "private", "sh", "-ec",
        "mount --bind \"$1/varlib\" /var/lib; mount --bind \"$1/etc\" /etc; mount --bind \"$1/somaxconn\" /proc/sys/net/core/somaxconn; KTUNER_FIX_PREVIOUS_CHILD=1 exec \"$2\" --exact fix_reports_the_recorded_previous --ignored --nocapture",
        "fix-previous-test"]).arg(&dir).arg(std::env::current_exe().unwrap()).output().unwrap();
    assert_eq!(fs::read_to_string(LIVE).unwrap(), before);
    let _ = fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
