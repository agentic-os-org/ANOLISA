use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

struct Tree(std::path::PathBuf);

impl Tree {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "ktuner-memory-hierarchy-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn write(&self, rel: &str, value: &str) {
        let path = self.0.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, value).unwrap();
    }

    fn v1_limit(&self, group: &str, bytes: &str) {
        self.write(&format!("memory/{group}/memory.limit_in_bytes"), bytes);
    }

    fn hierarchy(&self, group: &str, state: &str) {
        self.write(&format!("memory/{group}/memory.use_hierarchy"), state);
    }

    fn read(&self, membership: &str) -> u64 {
        cgroup_memory_limit_kb_from(&self.0, membership)
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn disabled_v1_parent_does_not_bind_the_leaf() {
    let t = Tree::new();
    t.v1_limit("", "9223372036854775807");
    t.hierarchy("", "0");
    t.v1_limit("parent", "2147483648");
    t.hierarchy("parent", "0");
    t.v1_limit("parent/leaf", "4294967296");
    t.hierarchy("parent/leaf", "0");

    let kb = t.read("11:memory:/parent/leaf\n");
    assert_eq!(kb, 4 * 1024 * 1024);
    assert_eq!(effective_memory_gb(64 * 1024 * 1024, kb), 4);

    // An unlimited leaf is not constrained by the disabled parent's limit.
    t.v1_limit("parent/leaf", "9223372036854775807");
    assert_eq!(t.read("11:memory:/parent/leaf\n"), 0);
}

#[test]
fn v1_accounting_stops_before_a_disabled_ancestor() {
    let t = Tree::new();
    t.v1_limit("", "9223372036854775807");
    t.hierarchy("", "0");
    t.v1_limit("a", "1073741824");
    t.hierarchy("a", "0");
    t.v1_limit("a/b", "2147483648");
    t.hierarchy("a/b", "1");
    t.v1_limit("a/b/c", "4294967296");
    t.hierarchy("a/b/c", "1");

    // c accounts to b, but a's counter is not in that accounting chain.
    assert_eq!(t.read("11:memory:/a/b/c\n"), 2 * 1024 * 1024);
}

#[test]
fn enabled_or_unknown_v1_hierarchy_keeps_ancestor_limits() {
    let t = Tree::new();
    t.v1_limit("", "9223372036854775807");
    t.v1_limit("parent", "2147483648");
    t.v1_limit("parent/leaf", "4294967296");
    for state in ["1\n", "", "garbage", "2"] {
        t.hierarchy("parent", state);
        assert_eq!(t.read("11:memory:/parent/leaf\n"), 2 * 1024 * 1024);
    }
    fs::remove_file(t.0.join("memory/parent/memory.use_hierarchy")).unwrap();
    assert_eq!(t.read("11:memory:/parent/leaf\n"), 2 * 1024 * 1024);
    fs::create_dir(t.0.join("memory/parent/memory.use_hierarchy")).unwrap();
    assert_eq!(t.read("11:memory:/parent/leaf\n"), 2 * 1024 * 1024);
}

#[test]
fn v2_and_hybrid_memory_navigation_keep_their_limits() {
    let t = Tree::new();
    t.v1_limit("", "9223372036854775807");
    t.v1_limit("parent", "2147483648");
    t.hierarchy("parent", "0");
    t.v1_limit("parent/leaf", "4294967296");
    assert_eq!(
        t.read("0::/unified\n11:memory:/parent/leaf\n"),
        4 * 1024 * 1024
    );

    t.write("memory.max", "max");
    t.write("unified/memory.max", "1073741824");
    assert_eq!(t.read("0::/unified\n11:memory:/parent/leaf\n"), 1024 * 1024);
    assert_eq!(t.read("0::/\n11:memory:/parent/leaf\n"), 0);
}
