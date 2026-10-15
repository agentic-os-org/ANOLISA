//! End-to-end coverage for the `system` section of `ktuner check`, driving
//! the real binary.
//!
//! `system` is the machine the engine diagnosed: an agent reads it to
//! identify the host behind the findings and to explain a recommendation the
//! rules did not make. Each disk entry carries the fields the disk rules
//! gate on, and each network entry is a name plus the link speed the network
//! rules threshold on. The whole contract is testable unprivileged: `check`
//! is read-only.

use std::process::Command;

fn check() -> serde_json::Value {
    let out = Command::new(env!("CARGO_BIN_EXE_ktuner"))
        .arg("check")
        .output()
        .expect("run ktuner check");
    assert!(
        matches!(out.status.code(), Some(0 | 1)),
        "ktuner check must exit 0 (nothing to advise) or 1 (recommendations): {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "ktuner check must print JSON on stdout: {e}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

/// What every disk entry carries: the disk identity plus the three fields
/// the disk rules gate on when they decide whether a disk-level suggestion
/// applies — `hidden` (a disk registered without a device number),
/// `holders` (a disk claimed exclusively by a dm/md/bcache/drbd member) and
/// `available_schedulers` (empty when the disk has no request queue).
const DISK_KEYS: [&str; 9] = [
    "available_schedulers",
    "hidden",
    "holders",
    "name",
    "nr_requests",
    "read_ahead_kb",
    "rq_affinity",
    "scheduler",
    "type",
];

#[test]
fn system_reports_the_host_identity_and_the_disk_and_link_inventory() {
    let body = check();
    let system = body["system"]
        .as_object()
        .unwrap_or_else(|| panic!("check must print a system object: {body}"));

    assert!(
        system["os_distro"].as_str().is_some_and(|v| !v.is_empty()),
        "the report identifies the distro it diagnosed: {body}"
    );
    assert!(
        system["cpu_model"].as_str().is_some_and(|v| !v.is_empty()),
        "the report names the CPU it read: {body}"
    );

    // The cpu and memory readings come in pairs: the effective value the
    // rules scale by and the host's own reading, which can never sit below
    // it (they are equal when no cgroup limit binds).
    assert!(system["cpu_cores"].is_u64(), "{body}");
    assert!(system["cpu_cores_host"].is_u64(), "{body}");
    assert!(system["memory_gb"].is_u64(), "{body}");
    assert!(system["memory_gb_host"].is_u64(), "{body}");
    assert!(
        system["cpu_cores_host"].as_u64() >= system["cpu_cores"].as_u64(),
        "the host's cpu count is at or above the effective count: {body}"
    );
    assert!(
        system["memory_gb_host"].as_u64() >= system["memory_gb"].as_u64(),
        "the host's memory is at or above the effective value: {body}"
    );

    for disk in system["disks"]
        .as_array()
        .unwrap_or_else(|| panic!("disks must be an array: {body}"))
    {
        let mut keys: Vec<&str> = disk
            .as_object()
            .unwrap_or_else(|| panic!("a disk entry is an object: {disk}"))
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys, DISK_KEYS,
            "a disk entry keeps its exact key set: {disk}"
        );
        assert!(
            disk["type"].as_str().is_some_and(|t| !t.is_empty()),
            "a disk entry names its type: {disk}"
        );
    }

    for link in system["network"]
        .as_array()
        .unwrap_or_else(|| panic!("network must be an array: {body}"))
    {
        let entry = link
            .as_object()
            .unwrap_or_else(|| panic!("a network entry is an object: {link}"));
        let mut keys: Vec<&str> = entry.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["name", "speed_mbps"],
            "a network entry keeps its exact key set: {link}"
        );
        assert!(
            entry["name"].as_str().is_some_and(|n| !n.is_empty()),
            "a network entry names its interface: {link}"
        );
        // 0 is the documented "unknown", not a speed of zero.
        assert!(
            entry["speed_mbps"].is_u64(),
            "a network entry reports a link speed: {link}"
        );
    }
}
