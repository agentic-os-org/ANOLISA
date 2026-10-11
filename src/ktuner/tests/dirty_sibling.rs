//! The dirty pair's cross-clearing semantics against files in a private mount
//! namespace: applying a `*_bytes` knob must record the ratio sibling the
//! kernel clears for it, and a rollback must bring that ratio back.
#![cfg(target_os = "linux")]

use ktuner_engine::{rules::Recommendation, tuner};
use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const LEDGER: &str = "/var/lib/ktuner/rollback.json";

fn rec(param: &str, value: &str) -> Recommendation {
    Recommendation {
        param: param.into(),
        current_value: "stale gathered value".into(),
        recommended_value: value.into(),
        writable: true,
        ..Default::default()
    }
}

fn ledger() -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(LEDGER).unwrap()).unwrap()
}

fn sysctl_path(param: &str) -> String {
    format!("/proc/sys/{}", param.replace('.', "/"))
}

fn body() {
    fs::create_dir_all("/var/lib/ktuner").unwrap();
    for (bytes_param, ratio_param, ratio_value) in [
        ("vm.dirty_bytes", "vm.dirty_ratio", "20"),
        (
            "vm.dirty_background_bytes",
            "vm.dirty_background_ratio",
            "10",
        ),
    ] {
        // Ratio mode host: bytes 0, a configured ratio the kernel will clear.
        fs::write(sysctl_path(bytes_param), "0").unwrap();
        fs::write(sysctl_path(ratio_param), ratio_value).unwrap();
        let _ = fs::remove_file(LEDGER);

        tuner::apply_one(&rec(bytes_param, "1073741824")).unwrap();

        // The knob itself is recorded as before.
        let data = ledger();
        assert_eq!(data["entries"][bytes_param]["previous"], "0");
        assert_eq!(data["entries"][bytes_param]["applied"], "1073741824");
        // ... and the sibling the kernel cleared is recorded with the value
        // that was live before the write, or the rollback below would leave
        // the host with a permanently zeroed ratio (throttling disabled).
        assert_eq!(
            data["entries"][ratio_param]["previous"], ratio_value,
            "the cleared ratio must be recorded before the write lands: {data}"
        );
        assert_eq!(
            data["entries"][ratio_param]["applied"], "0",
            "0 is what the kernel put live, exactly like a clamped read-back"
        );
        // Persistence reproduces the pair state from the bytes line alone.
        let persisted =
            fs::read_to_string("/etc/sysctl.d/99-ktuner.conf").expect("persisted sysctl.d");
        assert!(persisted.contains(&format!("{bytes_param} = 1073741824")));
        assert!(
            !persisted.contains(ratio_param),
            "the cleared-ratio record must not be re-applied at boot: {persisted}"
        );

        // The rollback restores BOTH knobs of the pair: bytes back to 0 and
        // the cleared ratio back to its configured value.
        assert!(tuner::rollback_quiet().unwrap().is_complete());
        assert_eq!(fs::read_to_string(sysctl_path(bytes_param)).unwrap(), "0");
        assert_eq!(
            fs::read_to_string(sysctl_path(ratio_param)).unwrap(),
            ratio_value,
            "rollback must bring the kernel-cleared ratio back"
        );
    }
    // A sibling that was already 0 loses nothing: no side-effect record.
    fs::write("/proc/sys/vm/dirty_bytes", "0").unwrap();
    fs::write("/proc/sys/vm/dirty_ratio", "0").unwrap();
    let _ = fs::remove_file(LEDGER);
    tuner::apply_one(&rec("vm.dirty_bytes", "1073741824")).unwrap();
    let data = ledger();
    assert!(
        data["entries"].get("vm.dirty_ratio").is_none(),
        "a zeroed sibling records nothing: {data}"
    );
    tuner::rollback_quiet().unwrap();
}

#[test]
#[ignore = "requires root and private mount namespaces; only fixture files are written"]
fn dirty_sibling_roundtrip() {
    if std::env::var_os("KTUNER_DIRTY_SIBLING_CHILD").is_some() {
        body();
        return;
    }
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "ktuner-dirty-sibling-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(dir.join("varlib")).unwrap();
    fs::create_dir_all(dir.join("etc/sysctl.d")).unwrap();
    fs::write(dir.join("dirty_bytes"), "0").unwrap();
    fs::write(dir.join("dirty_ratio"), "20").unwrap();
    fs::write(dir.join("dirty_background_bytes"), "0").unwrap();
    fs::write(dir.join("dirty_background_ratio"), "10").unwrap();
    let before_bytes = fs::read_to_string("/proc/sys/vm/dirty_bytes").unwrap();
    let before_ratio = fs::read_to_string("/proc/sys/vm/dirty_ratio").unwrap();
    let out = Command::new("unshare").args(["--mount", "--propagation", "private", "sh", "-ec",
        "mount --bind \"$1/varlib\" /var/lib; mount --bind \"$1/etc\" /etc; mount --bind \"$1/dirty_bytes\" /proc/sys/vm/dirty_bytes; mount --bind \"$1/dirty_ratio\" /proc/sys/vm/dirty_ratio; mount --bind \"$1/dirty_background_bytes\" /proc/sys/vm/dirty_background_bytes; mount --bind \"$1/dirty_background_ratio\" /proc/sys/vm/dirty_background_ratio; KTUNER_DIRTY_SIBLING_CHILD=1 exec \"$2\" --exact dirty_sibling_roundtrip --ignored --nocapture",
        "dirty-sibling-test"]).arg(&dir).arg(std::env::current_exe().unwrap()).output().unwrap();
    // The live host knobs are untouched: everything happened on fixtures.
    assert_eq!(
        fs::read_to_string("/proc/sys/vm/dirty_bytes").unwrap(),
        before_bytes
    );
    assert_eq!(
        fs::read_to_string("/proc/sys/vm/dirty_ratio").unwrap(),
        before_ratio
    );
    let _ = fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
