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

/// The cpuset controller bounds the same number the bandwidth quota does:
/// how many CPUs this process may actually run on. A Kubernetes pod with the
/// static CPU-manager policy, a `docker --cpuset-cpus=4-5` container, and a
/// systemd unit with `AllowedCPUs=` all cap the process while leaving the
/// quota unlimited, and /proc/cpuinfo still reports every host processor.
#[test]
fn cpuset_mask_bounds_the_effective_cpu_count() {
    let t = Tree::new();
    t.write("cpuset.cpus.effective", "0-63");
    t.write("kubepods/pod123/cpuset.cpus.effective", "4-5");
    t.write("kubepods/pod123/cpu.max", "max 100000");
    assert_eq!(t.read("0::/kubepods/pod123"), 2);

    // The mask is inherited: a cgroup that enables the controller without
    // narrowing it carries an equal or wider mask, so the smallest count on
    // the chain (here the pod's) is the one that binds.
    let t = Tree::new();
    t.write("system.slice/cpuset.cpus.effective", "4-7");
    t.write("system.slice/app.service/cpu.max", "max 100000");
    assert_eq!(t.read("0::/system.slice/app.service"), 4);

    // Whichever of the two limits is smaller binds.
    t.write("system.slice/app.service/cpu.max", "200000 100000");
    assert_eq!(t.read("0::/system.slice/app.service"), 2);
    let t = Tree::new();
    t.write("app/cpuset.cpus.effective", "0-3");
    t.write("app/cpu.max", "800000 100000");
    assert_eq!(t.read("0::/app"), 4);
}

#[test]
fn cpuset_mask_counts_ranges_and_singles() {
    let t = Tree::new();
    t.write("app/cpuset.cpus.effective", "0-3,8,10-11");
    assert_eq!(t.read("0::/app"), 7);

    // An empty or unparsable mask is not a CPU count, so it clamps nothing.
    for mask in ["", "\n", "garbage", "3-1", "0-"] {
        t.write("app/cpuset.cpus.effective", mask);
        assert_eq!(t.read("0::/app"), 0, "{mask:?}");
    }
}

#[test]
fn v1_cpuset_effective_mask_is_read_from_its_own_mount() {
    let t = Tree::new();
    t.write("cpuset/cpuset.effective_cpus", "0-3");
    t.write("cpuset/app.service/cpuset.effective_cpus", "0-1");
    assert_eq!(t.read("11:cpuset:/app.service"), 2);

    // Only the cpuset line's own mount carries the mask.
    let t = Tree::new();
    t.write("cpuset/cpuset.effective_cpus", "0-3");
    t.write("cpuset/app.service/cpuset.effective_cpus", "0-1");
    t.write("cpu/cpuset.effective_cpus", "0-63");
    assert_eq!(
        t.read("11:cpuset:/app.service\n3:cpu,cpuacct:/app.service"),
        2
    );
}
