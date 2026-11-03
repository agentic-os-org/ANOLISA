//! Original CLI inventory and rule output against read-only private fixtures.
#![cfg(target_os = "linux")]

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

type CpuCase<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)], u64);

struct Fixtures(PathBuf);
impl Drop for Fixtures {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn write(root: &Path, rel: &str, value: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, value).unwrap();
}

#[test]
#[ignore = "requires root and private mount namespaces; only read-only synthetic proc/cgroup mounts"]
fn cpu_hierarchy_cli_inventory_and_rules() {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixtures = Fixtures(
        std::env::temp_dir().join(format!("ktuner-cpu-cli-{}-{nonce}", std::process::id())),
    );
    let cases: &[CpuCase<'_>] = &[
        ("v2_flat", "0::/", &[("cpu.max", "100000 100000")], 1),
        (
            "v2_child",
            "0::/parent/leaf",
            &[
                ("parent/cpu.max", "max 100000"),
                ("parent/leaf/cpu.max", "100000 100000"),
            ],
            1,
        ),
        (
            "v2_parent",
            "0::/parent/leaf",
            &[
                ("parent/cpu.max", "100000 100000"),
                ("parent/leaf/cpu.max", "max 100000"),
            ],
            1,
        ),
        (
            "v2_parent_stricter",
            "0::/parent/leaf",
            &[
                ("parent/cpu.max", "100000 100000"),
                ("parent/leaf/cpu.max", "200000 100000"),
            ],
            1,
        ),
        (
            "v2_unlimited",
            "0::/parent/leaf",
            &[
                ("parent/cpu.max", "max 100000"),
                ("parent/leaf/cpu.max", "max 100000"),
            ],
            64,
        ),
        ("v2_fractional", "0::/", &[("cpu.max", "150000 100000")], 2),
        (
            "v2_root_stricter",
            "0::/parent/leaf",
            &[
                ("cpu.max", "100000 100000"),
                ("parent/cpu.max", "400000 100000"),
                ("parent/leaf/cpu.max", "200000 100000"),
            ],
            1,
        ),
        (
            "v2_sibling",
            "0::/parent/leaf",
            &[
                ("parent/cpu.max", "max 100000"),
                ("parent/leaf/cpu.max", "200000 100000"),
                ("parent/sibling/cpu.max", "100000 100000"),
            ],
            2,
        ),
        (
            "v1_flat",
            "3:cpu,cpuacct:/",
            &[
                ("cpu/cpu.cfs_quota_us", "200000"),
                ("cpu/cpu.cfs_period_us", "100000"),
            ],
            2,
        ),
        (
            "v1_parent",
            "3:cpu,cpuacct:/parent/leaf",
            &[
                ("cpu/cpu.cfs_quota_us", "-1"),
                ("cpu/cpu.cfs_period_us", "100000"),
                ("cpu/parent/cpu.cfs_quota_us", "100000"),
                ("cpu/parent/cpu.cfs_period_us", "100000"),
                ("cpu/parent/leaf/cpu.cfs_quota_us", "-1"),
                ("cpu/parent/leaf/cpu.cfs_period_us", "100000"),
            ],
            1,
        ),
    ];
    let live = "/proc/sys/kernel/sched_cfs_bandwidth_slice_us";
    let before = fs::read_to_string(live).unwrap();
    for (name, membership, files, expected) in cases {
        let dir = fixtures.0.join(name);
        fs::create_dir_all(dir.join("cgroup")).unwrap();
        write(&dir, "proc/self/cgroup", membership);
        let cpuinfo: String = (0..64)
            .map(|i| format!("processor : {i}\nmodel name : synthetic-cpu-test\n"))
            .collect();
        write(&dir, "proc/cpuinfo", &cpuinfo);
        write(
            &dir,
            "proc/meminfo",
            "MemTotal: 8388608 kB\nMemAvailable: 4194304 kB\n",
        );
        write(&dir, "proc/sys/kernel/osrelease", "6.8.0-synthetic");
        write(&dir, "proc/sys/kernel/sched_cfs_bandwidth_slice_us", "5000");
        for (path, value) in *files {
            write(&dir.join("cgroup"), path, value);
        }
        let out = Command::new("unshare").args(["--mount", "--propagation", "private", "sh", "-ec",
            "mount --bind \"$1/proc\" /proc; mount -o remount,bind,ro /proc; mount --bind \"$1/cgroup\" /sys/fs/cgroup; mount -o remount,bind,ro /sys/fs/cgroup; exec \"$2\" check", "cpu-test"])
            .arg(&dir).arg(env!("CARGO_BIN_EXE_ktuner")).output().unwrap();
        assert!(
            matches!(out.status.code(), Some(0 | 1)),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(result["system"]["cpu_cores"], *expected, "{name}");
        let recommendations = result["recommendations"].as_array().unwrap();
        let cpu_rule = recommendations
            .iter()
            .find(|r| r["param"] == "kernel.sched_cfs_bandwidth_slice_us");
        assert_eq!(
            cpu_rule.is_some(),
            *expected > 16,
            "{name}: {recommendations:?}"
        );
        println!(
            "{name}: cpu_cores={expected}, CPU threshold rule={}",
            cpu_rule.is_some()
        );
    }
    assert_eq!(fs::read_to_string(live).unwrap(), before);
}
