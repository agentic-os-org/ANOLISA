//! A library caller of the import path must be able to learn whether the boot
//! replay is armed. `tune`, `fix` and `rollback` all report it; a non-sysctl
//! value whose unit could not be enabled is live now and gone at the next
//! boot, and the caller is the only one left who can say so.
#![cfg(target_os = "linux")]

use ktuner_engine::tuner;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const LEDGER: &str = "/var/lib/ktuner/rollback.json";
/// A non-sysctl knob: the value goes to the generated script and its unit, so
/// the boot replay is what decides whether it survives a reboot.
const PARAM: &str = "transparent_hugepage/enabled";
const LIVE: &str = "/sys/kernel/mm/transparent_hugepage/enabled";
/// The fixture the child writes through, in the kernel's own shape: the
/// recorded original is read the way a real one would be.
const FIXTURE: &str = "always [madvise] never\n";

/// The body inside the fixture namespace: one import, its record, and the
/// verdict the two entry points answer with.
fn body() {
    let expect_armed = std::env::var("KTUNER_IMPORT_BOOT_UNIT_ARMED").unwrap() == "1";
    fs::create_dir_all("/var/lib/ktuner").unwrap();

    let armed = tuner::apply_import_reporting_boot_unit(PARAM, "never", None)
        .expect("the import must write the fixture knob");
    assert_eq!(
        armed, expect_armed,
        "the verdict must be the persistence's own"
    );
    assert_eq!(fs::read_to_string(LIVE).unwrap(), "never");

    // The record is written either way: the value is live from now on and
    // `rollback` undoes it. The verdict says only whether it comes back after
    // a reboot, which is exactly the fact a run whose `systemctl enable`
    // failed cannot leave unsaid.
    let data: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(LEDGER).unwrap()).unwrap();
    assert_eq!(data["entries"][PARAM]["applied"], "never");
    assert_eq!(data["entries"][PARAM]["previous"], "madvise");

    // The entry point that drops the verdict keeps its contract: the same
    // write, and no error to catch — a caller that does not look at the boot
    // replay is not newly refused anything.
    tuner::apply_import(PARAM, "always", None).unwrap();
    assert_eq!(fs::read_to_string(LIVE).unwrap(), "always");
    let data: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(LEDGER).unwrap()).unwrap();
    assert_eq!(
        data["entries"][PARAM]["previous"], "madvise",
        "the second write keeps the first recorded original: {data}"
    );
}

/// One scenario: a fixture namespace whose `systemctl` stand-in accepts
/// (`armed`) or refuses the enable.
fn run_scenario(armed: bool) -> (std::path::PathBuf, std::process::Output) {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "ktuner-import-boot-unit-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(dir.join("varlib")).unwrap();
    fs::create_dir_all(dir.join("etc/systemd/system")).unwrap();
    fs::create_dir_all(dir.join("etc/sysctl.d")).unwrap();
    fs::create_dir_all(dir.join("bin")).unwrap();
    fs::write(dir.join("thp-enabled"), FIXTURE).unwrap();
    let stand_in = dir.join("bin/systemctl");
    fs::write(
        &stand_in,
        if armed {
            "#!/bin/sh\nexit 0\n"
        } else {
            "#!/bin/sh\nexit 1\n"
        },
    )
    .unwrap();
    fs::set_permissions(&stand_in, fs::Permissions::from_mode(0o755)).unwrap();

    let out = Command::new("unshare").args(["--mount", "--propagation", "private", "sh", "-ec",
        "mount --bind \"$1/varlib\" /var/lib; mount --bind \"$1/etc\" /etc; mount --bind \"$1/thp-enabled\" /sys/kernel/mm/transparent_hugepage/enabled; export PATH=\"$1/bin:$PATH\"; KTUNER_IMPORT_BOOT_UNIT_CHILD=1 KTUNER_IMPORT_BOOT_UNIT_ARMED=\"$3\" exec \"$2\" --exact a_library_import_reports_the_boot_replay_verdict --ignored --nocapture",
        "import-boot-unit-test"])
        .arg(&dir)
        .arg(std::env::current_exe().unwrap())
        .arg(if armed { "1" } else { "0" })
        .output()
        .unwrap();
    (dir, out)
}

#[test]
#[ignore = "requires root and private mount namespaces; only fixture files are written"]
fn a_library_import_reports_the_boot_replay_verdict() {
    if std::env::var_os("KTUNER_IMPORT_BOOT_UNIT_CHILD").is_some() {
        body();
        return;
    }
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let before = fs::read_to_string(LIVE).unwrap();

    for armed in [true, false] {
        let (dir, out) = run_scenario(armed);
        let _ = fs::remove_dir_all(&dir);
        assert!(
            out.status.success(),
            "armed={armed}\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // The live host knob is untouched: everything happened on fixture files.
    assert_eq!(fs::read_to_string(LIVE).unwrap(), before);
}
