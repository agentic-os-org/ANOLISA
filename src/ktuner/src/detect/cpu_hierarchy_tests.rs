use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

struct Tree(std::path::PathBuf);
impl Tree {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("ktuner-cpu-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, rel: &str, value: &str) {
        let path = self.0.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, value).unwrap();
    }
    fn v1(&self, dir: &str, quota: &str, period: &str) {
        self.write(&format!("{dir}/cpu.cfs_quota_us"), quota);
        self.write(&format!("{dir}/cpu.cfs_period_us"), period);
    }
    fn read(&self, membership: &str) -> u64 {
        cgroup_cpu_limit_cores_from(&self.0, membership)
    }
}
impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn nested_cpu_limits_use_own_chain_not_siblings() {
    let t = Tree::new();
    t.write("parent/cpu.max", "max 100000");
    t.write("parent/leaf/cpu.max", "200000 100000");
    t.write("parent/sibling/cpu.max", "100000 100000");
    assert_eq!(t.read("0::/parent/leaf"), 2);
    t.write("parent/cpu.max", "100000 100000");
    assert_eq!(t.read("0::/parent/leaf"), 1);
    t.write("parent/leaf/cpu.max", "max 100000");
    assert_eq!(t.read("0::/parent/leaf"), 1);
}

#[test]
fn cpu_ancestors_compare_quota_ratios_with_existing_ceil() {
    let t = Tree::new();
    t.write("cpu.max", "400000 100000");
    t.write("parent/cpu.max", "150000 100000");
    t.write("parent/leaf/cpu.max", "200000 200000");
    assert_eq!(t.read("0::/parent/leaf"), 1);
    t.write("parent/leaf/cpu.max", "max 100000");
    assert_eq!(t.read("0::/parent/leaf"), 2);
}

#[test]
fn hybrid_cpu_fallback_uses_cpu_membership_not_memory() {
    let t = Tree::new();
    t.v1("cpu/parent", "100000", "100000");
    t.v1("cpu/parent/leaf", "-1", "100000");
    t.v1("cpu/wrong", "400000", "100000");
    assert_eq!(
        t.read("11:memory:/wrong\n3:cpu,cpuacct:/parent/leaf\n0::/unified"),
        1
    );
    // An actual unlimited v2 CPU chain is authoritative over v1.
    t.write("unified/cpu.max", "max 100000");
    assert_eq!(t.read("3:cpu,cpuacct:/parent/leaf\n0::/unified"), 0);
}

#[test]
fn v1_combined_mount_and_ancestor_limit_are_supported() {
    for mount in ["cpu,cpuacct", "cpuacct,cpu"] {
        let t = Tree::new();
        t.v1(mount, "-1", "100000");
        t.v1(&format!("{mount}/parent"), "150000", "100000");
        t.v1(&format!("{mount}/parent/leaf"), "400000", "100000");
        assert_eq!(t.read("3:cpuacct,cpu:/parent/leaf"), 2);
    }
}

#[test]
fn cpu_missing_or_malformed_inputs_keep_fallback_contract() {
    let t = Tree::new();
    assert_eq!(t.read("0::/missing"), 0);
    t.write("cpu.max", "200000 100000");
    for membership in ["", "broken", "0::/", "0::/missing", "0::/../../outside"] {
        assert_eq!(t.read(membership), 2, "{membership}");
    }
    t.write("parent/cpu.max", "100000 100000");
    t.write("parent/leaf/cpu.max", "garbage");
    assert_eq!(t.read("0::/parent/leaf"), 1);
    fs::remove_file(t.0.join("cpu.max")).unwrap();
    t.v1("cpu", "200000", "100000");
    assert_eq!(t.read("0::/missing"), 2);
}
