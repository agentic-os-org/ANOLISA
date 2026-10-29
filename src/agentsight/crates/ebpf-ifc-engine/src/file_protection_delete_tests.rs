use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use super::*;

struct Fixture {
    root: PathBuf,
    pins: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let id = format!(
            "fp-delete-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        );
        let root = PathBuf::from("/dev/shm").join(&id);
        let pins = PathBuf::from("/sys/fs/bpf").join(&id);
        fs::create_dir(&root).expect("create private fixture");
        // SAFETY: ignored live suites are explicitly run serially in their own test process.
        unsafe {
            std::env::set_var("ACTPLANE_BPF_PIN_ROOT", &pins);
            std::env::set_var("ACTPLANE_PINNED_PROFILE", "agent-file-guard");
        }
        Self { root, pins }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove private fixture");
        if self.pins.exists() {
            fs::remove_dir_all(&self.pins).expect("remove private pins");
        }
    }
}

struct Worker {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Worker {
    fn new() -> Self {
        let mut child = Command::new("unshare")
            .args([
                "-n",
                "python3",
                "-u",
                "-c",
                r#"
import os, sys
print('ready', flush=True)
for line in sys.stdin:
    op, path = line.rstrip('\n').split('|', 1)
    try:
        if op == 'unlink':
            os.unlink(path)
        elif op == 'rmdir':
            os.rmdir(path)
        else:
            raise ValueError(op)
        print('ok', flush=True)
    except OSError as error:
        print('errno=' + str(error.errno), flush=True)
"#,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn private worker");
        let input = child.stdin.take().expect("stdin");
        let mut output = BufReader::new(child.stdout.take().expect("stdout"));
        let mut ready = String::new();
        output.read_line(&mut ready).expect("worker ready");
        assert_eq!(ready.trim(), "ready");
        Self {
            child,
            input,
            output,
        }
    }

    fn run(&mut self, operation: &str, path: &Path) -> String {
        writeln!(self.input, "{operation}|{}", path.display()).expect("worker command");
        self.input.flush().expect("flush command");
        let mut response = String::new();
        assert_ne!(self.output.read_line(&mut response).expect("response"), 0);
        response.trim().into()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn identity(path: &Path) -> TreePathKey {
    let metadata = fs::metadata(path).expect("fixture identity");
    TreePathKey {
        ino: metadata.ino(),
        dev: (libc::major(metadata.dev()) << 20) | libc::minor(metadata.dev()),
        mnt_id: 0,
        domain_id: 77,
        generation: 1,
    }
}

fn slot(parent: &Path, name: &str) -> PolicySlotKey {
    let mut bytes = [0; FILE_PROTECTION_NAME_MAX + 1];
    bytes[..name.len()].copy_from_slice(name.as_bytes());
    PolicySlotKey {
        parent: identity(parent),
        name_len: name.len() as u32,
        name: bytes,
        _pad: 0,
    }
}

#[test]
#[ignore = "requires root, BPF LSM, unlimited memlock and --test-threads=1; private pins only"]
fn structured_deletion_classifier_on_live_kernel() {
    let fixture = Fixture::new();
    let tree = fixture.root.join("tree");
    let cache = tree.join("cache");
    let empty = tree.join("empty");
    fs::create_dir_all(&cache).expect("excluded subtree");
    fs::create_dir(&empty).expect("empty protected directory");
    let mut deep = tree.clone();
    for _ in 0..260 {
        deep.push("d");
        fs::create_dir(&deep).expect("deep directory");
    }
    let deep_file = deep.join("entry");
    let deep_empty = deep.join("empty");
    let future = tree.join("future");
    let outside = fixture.root.join("outside");
    let exact = fixture.root.join("exact");
    let excluded = cache.join("excluded");
    let nested = cache.join("nested-direct");
    for path in [&deep_file, &outside, &exact, &excluded, &nested] {
        fs::write(path, b"fixture").expect("file fixture");
    }
    fs::create_dir(&deep_empty).expect("deep empty directory");
    let engine = PinnedEngine::open_or_install_singleton().expect("load private engine");
    let mut worker = Worker::new();
    engine
        .seed_label_in_domain(worker.child.id() as i32, 77, 1)
        .expect("seed worker");
    let mut anchors =
        pinned_hash_map::<TreePathKey, PolicyRootVal>(&engine.paths, "te_policy_dirs")
            .expect("directory anchors");
    anchors
        .insert(identity(&tree), PolicyRootVal { root_mask: 1 }, BPF_NOEXIST)
        .expect("delete tree");
    anchors
        .insert(identity(&deep), PolicyRootVal { root_mask: 2 }, BPF_NOEXIST)
        .expect("near write-only tree");
    let mut slots =
        pinned_hash_map::<PolicySlotKey, PolicySlotVal>(&engine.paths, "te_policy_slots")
            .expect("slot map");
    slots
        .insert(
            slot(&fixture.root, "exact"),
            PolicySlotVal {
                direct_mask: 4,
                exact_exclude_mask: 0,
                subtree_exclude_mask: 0,
            },
            BPF_NOEXIST,
        )
        .expect("exact slot");
    slots
        .insert(
            slot(&tree, "cache"),
            PolicySlotVal {
                direct_mask: 0,
                exact_exclude_mask: 0,
                subtree_exclude_mask: 1,
            },
            BPF_NOEXIST,
        )
        .expect("excluded subtree slot");
    slots
        .insert(
            slot(&cache, "nested-direct"),
            PolicySlotVal {
                direct_mask: 4,
                exact_exclude_mask: 0,
                subtree_exclude_mask: 0,
            },
            BPF_NOEXIST,
        )
        .expect("direct target inside exclusion");
    let mut domains = pinned_hash_map::<u32, PolicyDomainVal>(&engine.paths, "te_policy_domains")
        .expect("activation map");
    let mut domain = PolicyDomainVal {
        generation: 1,
        target_count: 3,
        tree_mask: 3,
        delete_mask: 5,
        ordinary_write_mask: 2,
        protection_set_id: 1,
        guard: InodeGuardVal {
            flags: INODE_GUARD_UNLINK,
            rule_id: 1,
            domain_id: 77,
            _pad: 0,
        },
    };
    domains
        .insert(77, domain, BPF_NOEXIST)
        .expect("activate last");
    fs::write(&future, b"future entry").expect("new descendant after activation");
    for (operation, path) in [
        ("unlink", &future),
        ("unlink", &exact),
        ("unlink", &nested),
        ("unlink", &deep_file),
        ("rmdir", &empty),
        ("rmdir", &deep_empty),
    ] {
        assert_eq!(
            worker.run(operation, path),
            "errno=1",
            "{operation}: {}",
            path.display()
        );
        assert!(path.exists(), "denial must preserve the entry");
    }
    assert_eq!(worker.run("unlink", &excluded), "ok");
    assert_eq!(worker.run("unlink", &outside), "ok");
    // A domain without delete protection must not fail-close even deep UNKNOWN paths.
    domain.delete_mask = 0;
    domains
        .insert(77, domain, BPF_ANY)
        .expect("switch to write-only mask");
    assert_eq!(worker.run("unlink", &deep_file), "ok");
    assert_eq!(worker.run("rmdir", &deep_empty), "ok");
    domains.remove(&77).expect("deactivate before cleanup");
    assert_eq!(worker.run("unlink", &future), "ok");
}
