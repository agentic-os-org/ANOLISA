//! Apply and rollback contracts against files in a private mount namespace.
#![cfg(target_os = "linux")]

use ktuner_engine::{rules::Recommendation, tuner};
use std::fs;
use std::os::fd::AsRawFd;
use std::process::Command;
use std::sync::{Arc, Barrier};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LEDGER: &str = "/var/lib/ktuner/rollback.json";
const PARAM: &str = "net.core.somaxconn";
const LIVE: &str = "/proc/sys/net/core/somaxconn";

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

fn seed() {
    fs::write(LIVE, "1024").unwrap();
    fs::write(
        LEDGER,
        serde_json::json!({"version": 1, "entries": {
            PARAM: {"previous": "128", "applied": "1024", "path": LIVE}
        }})
        .to_string(),
    )
    .unwrap();
}

fn body() {
    fs::create_dir_all("/var/lib/ktuner").unwrap();
    // All high-level apply entrances must wait BEFORE changing a parameter.
    for mode in ["one", "quiet", "batch", "import"] {
        seed();
        let lock = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(format!("{LEDGER}.lock"))
            .unwrap();
        assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
        let ready = Arc::new(Barrier::new(2));
        let worker_ready = ready.clone();
        let worker = std::thread::spawn(move || {
            worker_ready.wait();
            match mode {
                "one" => {
                    tuner::apply_one(&rec(PARAM, "65535")).unwrap();
                }
                "quiet" => {
                    assert_eq!(
                        tuner::apply_quiet(&[rec(PARAM, "65535")]).unwrap().applied,
                        1
                    );
                }
                "batch" => {
                    assert_eq!(tuner::apply(&[rec(PARAM, "65535")]).unwrap().applied, 1);
                }
                _ => {
                    tuner::apply_import(PARAM, "65535", Some("1024")).unwrap();
                }
            }
        });
        ready.wait();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            fs::read_to_string(LIVE).unwrap(),
            "1024",
            "{mode}: write escaped the transaction lock"
        );
        assert_eq!(ledger()["entries"][PARAM]["previous"], "128");
        drop(lock);
        worker.join().unwrap();
        assert_eq!(fs::read_to_string(LIVE).unwrap(), "65535");
        assert_eq!(ledger()["entries"][PARAM]["previous"], "128");
        assert!(fs::read_to_string("/etc/sysctl.d/99-ktuner.conf")
            .unwrap()
            .contains("65535"));
        assert!(tuner::rollback_quiet().unwrap().is_complete());
        assert_eq!(fs::read_to_string(LIVE).unwrap(), "128");
    }
    // Both serial orders: fix then rollback above; rollback then stale fix here.
    seed();
    tuner::rollback_quiet().unwrap();
    tuner::apply_one(&rec(PARAM, "4096")).unwrap();
    assert_eq!(ledger()["entries"][PARAM]["previous"], "128");
    tuner::apply_import(PARAM, "8192", Some("incorrect caller snapshot")).unwrap();
    assert_eq!(ledger()["entries"][PARAM]["previous"], "128");
    tuner::rollback_quiet().unwrap();
    assert_eq!(fs::read_to_string(LIVE).unwrap(), "128");

    // A missing hint must not suppress a readable original.
    tuner::apply_import(PARAM, "4096", None).unwrap();
    assert_eq!(ledger()["entries"][PARAM]["previous"], "128");
    tuner::rollback_quiet().unwrap();

    for other in [PARAM, "vm.swappiness"] {
        seed();
        fs::write("/proc/sys/vm/swappiness", "60").unwrap();
        let a = std::thread::spawn(|| tuner::apply_one(&rec(PARAM, "4096")).unwrap());
        let b = std::thread::spawn(move || tuner::apply_one(&rec(other, "8192")).unwrap());
        a.join().unwrap();
        b.join().unwrap();
        let data = ledger();
        assert_eq!(data["entries"][PARAM]["previous"], "128");
        assert_eq!(
            data["entries"][PARAM]["applied"],
            fs::read_to_string(LIVE).unwrap()
        );
        if other != PARAM {
            assert_eq!(data["entries"][other]["previous"], "60");
        }
        assert!(tuner::rollback_quiet().unwrap().is_complete());
        assert_eq!(fs::read_to_string(LIVE).unwrap(), "128");
    }
    // Successful batch members are reversible even when another member fails.
    let outcome = tuner::apply_quiet(&[rec(PARAM, "4096"), rec("vm.ktuner_absent", "10")]).unwrap();
    assert_eq!(outcome.applied, 1);
    assert_eq!(outcome.failed.len(), 1);
    assert_eq!(ledger()["entries"].as_object().unwrap().len(), 1);
    tuner::rollback_quiet().unwrap();
    fs::write(LEDGER, "{broken").unwrap();
    for mode in ["one", "batch", "import"] {
        let error = match mode {
            "one" => tuner::apply_one(&rec(PARAM, "4096")).map(|_| ()),
            "batch" => tuner::apply_quiet(&[rec(PARAM, "4096")]).map(|_| ()),
            _ => tuner::apply_import(PARAM, "4096", Some("128")),
        };
        assert!(error.is_err());
        assert_eq!(fs::read_to_string(LIVE).unwrap(), "128");
        assert_eq!(fs::read_to_string(LEDGER).unwrap(), "{broken");
    }
}

#[test]
#[ignore = "requires root and private mount namespaces; only fixture files are written"]
fn parameter_transactions() {
    if std::env::var_os("KTUNER_TRANSACTION_CHILD").is_some() {
        body();
        return;
    }
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "ktuner-transactions-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(dir.join("varlib")).unwrap();
    fs::create_dir_all(dir.join("etc/sysctl.d")).unwrap();
    fs::write(dir.join("somaxconn"), "128").unwrap();
    fs::write(dir.join("swappiness"), "60").unwrap();
    let before = fs::read_to_string(LIVE).unwrap();
    let out = Command::new("unshare").args(["--mount", "--propagation", "private", "sh", "-ec",
        "mount --bind \"$1/varlib\" /var/lib; mount --bind \"$1/etc\" /etc; mount --bind \"$1/somaxconn\" /proc/sys/net/core/somaxconn; mount --bind \"$1/swappiness\" /proc/sys/vm/swappiness; KTUNER_TRANSACTION_CHILD=1 exec \"$2\" --exact parameter_transactions --ignored --nocapture",
        "transaction-test"]).arg(&dir).arg(std::env::current_exe().unwrap()).output().unwrap();
    assert_eq!(fs::read_to_string(LIVE).unwrap(), before);
    let _ = fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
