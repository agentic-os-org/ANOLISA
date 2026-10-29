#![cfg(all(feature = "actplane", target_os = "linux"))]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use agentsight_enforcement_protocol::{
    ApplyCredentialPolicy, ApplyPolicy, CredentialExfiltrationPolicy, DestinationScope,
    FILE_PROTECTION_POLICY_VERSION, FileProtectionExclusion, FileProtectionExclusionKind,
    FileProtectionPolicy, FileProtectionTarget, FileProtectionTargetKind,
    FileProtectionTopologyMode, FileProtections, PolicyMode, SecurityEvent, SecurityEventKind,
};
use agentsight_enforcer::{ActPlaneBackend, EnforcementBackend, SubscriberClass};
use uuid::Uuid;

const WORKER: &str = r#"
import errno, os, socket, sys
path = sys.argv[1]
fd = os.open(path, os.O_RDONLY)
print('ready', flush=True)
for line in sys.stdin:
    op = line.strip()
    try:
        if op == 'unlink':
            os.unlink(path)
        elif op == 'rename':
            os.rename(path, path + '.renamed')
        elif op == 'readfd':
            os.lseek(fd, 0, os.SEEK_SET)
            os.read(fd, 4096)
        elif op == 'read':
            with open(path, 'rb') as f:
                f.read()
        elif op == 'mmap':
            import mmap
            with mmap.mmap(fd, 0, access=mmap.ACCESS_READ) as m:
                m.read()
        elif op.startswith('connect '):
            with socket.socket() as s:
                s.setblocking(False)
                s.connect_ex((op.split()[1], 9))
        elif op == 'exit':
            break
        else:
            raise ValueError(op)
        print('ok', flush=True)
    except OSError as e:
        print('errno=' + str(e.errno), flush=True)
"#;

struct Worker {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Worker {
    fn spawn(path: &str) -> Self {
        let mut child = Command::new("unshare")
            .args(["-n", "python3", "-u", "-c", WORKER, path])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn network-isolated test process");
        let input = child.stdin.take().expect("child stdin");
        let mut output = BufReader::new(child.stdout.take().expect("child stdout"));
        let mut ready = String::new();
        output.read_line(&mut ready).expect("worker ready");
        assert_eq!(ready.trim(), "ready");
        Self {
            child,
            input,
            output,
        }
    }

    fn pid(&self) -> i32 {
        self.child.id() as i32
    }

    fn start_time(&self) -> u64 {
        fs::read_to_string(format!("/proc/{}/stat", self.pid()))
            .expect("process stat")
            .rsplit_once(") ")
            .expect("comm terminator")
            .1
            .split_whitespace()
            .nth(19)
            .expect("start time")
            .parse()
            .expect("numeric start time")
    }

    fn run(&mut self, command: &str) -> String {
        writeln!(self.input, "{command}").expect("worker command");
        self.input.flush().expect("flush command");
        let mut result = String::new();
        self.output.read_line(&mut result).expect("worker result");
        result.trim().to_owned()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = writeln!(self.input, "exit");
        let _ = self.input.flush();
        let _ = self.child.wait();
    }
}

const TREE_WORKER: &str = r#"
import ctypes, mmap, os, sys
libc = ctypes.CDLL(None, use_errno=True)
openfds = {}
for line in sys.stdin:
    parts = line.rstrip('\n').split('\t')
    op = parts[0]
    try:
        if op == 'touch':
            with open(parts[1], 'wb') as f:
                f.write(b'fixture')
        elif op == 'create_empty':
            os.close(os.open(parts[1], os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600))
        elif op == 'mkdir':
            os.mkdir(parts[1])
        elif op == 'bind':
            os.makedirs(parts[2], exist_ok=True)
            rc = libc.mount(parts[1].encode(), parts[2].encode(), None, 4096, None)
            if rc != 0:
                value = ctypes.get_errno()
                raise OSError(value, os.strerror(value))
        elif op == 'unlink':
            os.unlink(parts[1])
        elif op == 'openfd':
            openfds[parts[1]] = os.open(parts[1], os.O_RDWR)
        elif op == 'writefd':
            os.write(openfds[parts[1]], b'-writefd')
        elif op == 'pwritefd':
            os.pwrite(openfds[parts[1]], b'pwritefd', 0)
        elif op == 'readfd':
            os.pread(openfds[parts[1]], 4096, 0)
        elif op == 'write':
            with open(parts[1], 'ab') as f:
                f.write(b'-write')
        elif op == 'pwrite':
            fd = os.open(parts[1], os.O_RDWR)
            try:
                os.pwrite(fd, b'pwrite', 0)
            finally:
                os.close(fd)
        elif op == 'read':
            with open(parts[1], 'rb') as f:
                f.read()
        elif op == 'truncate':
            os.truncate(parts[1], 1)
        elif op == 'otrunc':
            os.close(os.open(parts[1], os.O_WRONLY | os.O_TRUNC))
        elif op == 'mmap_write':
            with open(parts[1], 'r+b') as f:
                with mmap.mmap(f.fileno(), 0, flags=mmap.MAP_SHARED,
                               prot=mmap.PROT_READ | mmap.PROT_WRITE) as mapped:
                    mapped[0:1] = b'M'
                    mapped.flush()
        elif op == 'rmdir':
            os.rmdir(parts[1])
        elif op == 'rename':
            os.rename(parts[1], parts[2])
        elif op in ('exchange', 'noreplace'):
            flags = 2 if op == 'exchange' else 1
            rc = libc.renameat2(-100, parts[1].encode(), -100, parts[2].encode(), flags)
            if rc != 0:
                value = ctypes.get_errno()
                raise OSError(value, os.strerror(value))
        elif op == 'exit':
            break
        else:
            raise ValueError(op)
        print('ok', flush=True)
    except OSError as e:
        print('errno=' + str(e.errno), flush=True)
for fd in openfds.values():
    os.close(fd)
"#;

struct TreeWorker {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl TreeWorker {
    fn spawn_private_mount() -> Self {
        let mut command = Command::new("unshare");
        command.args(["--mount", "--propagation", "private", "python3"]);
        Self::spawn_command(&mut command)
    }

    fn spawn_command(command: &mut Command) -> Self {
        let mut child = command
            .args(["-u", "-c", TREE_WORKER])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn tree test process");
        Self {
            input: child.stdin.take().expect("child stdin"),
            output: BufReader::new(child.stdout.take().expect("child stdout")),
            child,
        }
    }

    fn pid(&self) -> i32 {
        self.child.id() as i32
    }

    fn start_time(&self) -> u64 {
        fs::read_to_string(format!("/proc/{}/stat", self.pid()))
            .expect("process stat")
            .rsplit_once(") ")
            .expect("comm terminator")
            .1
            .split_whitespace()
            .nth(19)
            .expect("start time")
            .parse()
            .expect("numeric start time")
    }

    fn snapshot(path: &str) -> Option<(u64, u64, Vec<u8>)> {
        fs::metadata(path).ok().map(|metadata| {
            let content = if metadata.is_file() {
                fs::read(path).expect("snapshot content")
            } else {
                let mut entries: Vec<_> = fs::read_dir(path)
                    .expect("snapshot directory")
                    .map(|entry| entry.expect("directory entry").file_name())
                    .collect();
                entries.sort();
                let descendants: Vec<_> = entries
                    .iter()
                    .map(|name| {
                        let child = std::path::Path::new(path).join(name);
                        (
                            name,
                            Self::snapshot(child.to_str().expect("ASCII fixture path")),
                        )
                    })
                    .collect();
                format!("{descendants:?}").into_bytes()
            };
            (metadata.dev(), metadata.ino(), content)
        })
    }

    fn run(&mut self, operation: &str, paths: &[&str]) -> String {
        let before: Vec<_> = paths.iter().map(|path| Self::snapshot(path)).collect();
        write!(self.input, "{operation}").expect("worker operation");
        for path in paths {
            write!(self.input, "\t{path}").expect("worker path");
        }
        writeln!(self.input).expect("worker command");
        self.input.flush().expect("flush command");
        let mut result = String::new();
        self.output.read_line(&mut result).expect("worker result");
        let result = result.trim().to_owned();
        let after: Vec<_> = paths.iter().map(|path| Self::snapshot(path)).collect();
        if result.starts_with("errno=") {
            if operation == "touch" && before[0].is_none() {
                assert_eq!(
                    after[0].as_ref().map(|entry| entry.2.as_slice()),
                    Some(b"".as_slice()),
                    "creation must complete before the first write is denied"
                );
            } else {
                assert_eq!(before, after, "denied {operation} changed inode/content");
            }
        } else if result == "ok" {
            match operation {
                "rename" | "noreplace" => {
                    assert!(after[0].is_none(), "rename left source behind");
                    assert_eq!(after[1], before[0], "rename lost source identity/content");
                }
                "exchange" => {
                    assert_eq!(after[0], before[1], "exchange source identity/content");
                    assert_eq!(after[1], before[0], "exchange target identity/content");
                }
                "unlink" | "rmdir" => assert!(after[0].is_none()),
                "touch" | "create_empty" | "mkdir" => assert!(after[0].is_some()),
                "bind" => assert!(after[1].is_some()),
                "read" | "readfd" | "openfd" => assert_eq!(after, before),
                "write" | "pwrite" | "writefd" | "pwritefd" | "truncate" | "otrunc"
                | "mmap_write" => {
                    assert_eq!(
                        after[0].as_ref().map(|entry| (entry.0, entry.1)),
                        before[0].as_ref().map(|entry| (entry.0, entry.1))
                    );
                    assert_ne!(after[0], before[0]);
                }
                _ => panic!("unexpected operation {operation}"),
            }
        }
        result
    }
}

impl Drop for TreeWorker {
    fn drop(&mut self) {
        let _ = writeln!(self.input, "exit");
        let _ = self.input.flush();
        let _ = self.child.wait();
    }
}

struct Files {
    root: PathBuf,
    pin_root: PathBuf,
}

impl Files {
    fn new(profile: &str) -> Self {
        let name = format!("ap-{}", &Uuid::new_v4().simple().to_string()[..10]);
        let root = PathBuf::from("/tmp").join(&name);
        let pin_root = PathBuf::from("/sys/fs/bpf").join(name);
        fs::create_dir(&root).expect("test directory");
        fs::write(
            root.join("source"),
            b"synthetic fixture, not a credential\n",
        )
        .expect("source fixture");
        // This ignored test must run alone; runtime threads are joined before the next profile.
        unsafe {
            std::env::set_var("ACTPLANE_PINNED_PROFILE", profile);
            std::env::set_var("ACTPLANE_BPF_PIN_ROOT", &pin_root);
        }
        Self { root, pin_root }
    }

    fn source(&self) -> String {
        self.root
            .join("source")
            .to_str()
            .expect("ASCII path")
            .to_owned()
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.pin_root);
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct TreeFiles {
    outer: PathBuf,
    root: PathBuf,
    outside: PathBuf,
    pin_root: PathBuf,
}

impl TreeFiles {
    fn new() -> Self {
        let name = format!("ap-tree-{}", &Uuid::new_v4().simple().to_string()[..10]);
        let outer = PathBuf::from("/dev/shm").join(&name);
        let root = outer.join("protected");
        let outside = outer.join("outside");
        let pin_root = PathBuf::from("/sys/fs/bpf").join(name);
        fs::create_dir_all(root.join("nested")).expect("protected tree");
        fs::create_dir(&outside).expect("outside directory");
        fs::write(root.join("existing"), b"existing").expect("existing descendant");
        fs::write(root.join("nested/child"), b"nested").expect("nested descendant");
        fs::write(outside.join("marker"), b"marker").expect("binding marker");
        unsafe {
            std::env::set_var("ACTPLANE_PINNED_PROFILE", "agent-file-guard");
            std::env::set_var("ACTPLANE_BPF_PIN_ROOT", &pin_root);
        }
        Self {
            outer,
            root,
            outside,
            pin_root,
        }
    }

    fn text(path: &std::path::Path) -> &str {
        path.to_str().expect("ASCII fixture path")
    }
}

impl Drop for TreeFiles {
    fn drop(&mut self) {
        // Runtime lock files are shared and must not be removed by a pin fixture.
        let _ = fs::remove_dir_all(&self.pin_root);
        let _ = fs::remove_dir_all(&self.outer);
    }
}

fn structured_policy_classifier() {
    let files = TreeFiles::new();
    let mut worker = TreeWorker::spawn_private_mount();
    let bind_alias = files.outside.join("bind-alias");
    assert_eq!(
        worker.run(
            "bind",
            &[TreeFiles::text(&files.root), TreeFiles::text(&bind_alias)]
        ),
        "ok"
    );
    let external_mount_source = files.outside.join("external-source");
    let protected_submount = files.root.join("nested/mounted");
    fs::create_dir(&external_mount_source).expect("create external mount source");
    fs::write(external_mount_source.join("entry"), b"mounted").expect("mounted entry");
    assert_eq!(
        worker.run(
            "bind",
            &[
                TreeFiles::text(&external_mount_source),
                TreeFiles::text(&protected_submount),
            ]
        ),
        "ok"
    );
    let real_parent = files.outer.join("real");
    let real_tree = real_parent.join("tree");
    fs::create_dir_all(&real_tree).expect("create canonical symlink target tree");
    let intermediate_alias = files.outside.join("alias");
    std::os::unix::fs::symlink(&real_parent, &intermediate_alias)
        .expect("create intermediate target symlink");
    let authored_alias_tree = intermediate_alias.join("tree");
    let alias_mount_source = files.outside.join("alias-mount-source");
    let alias_submount = real_tree.join("mounted");
    fs::create_dir(&alias_mount_source).expect("create aliased-tree mount source");
    fs::write(alias_mount_source.join("entry"), b"mounted through alias")
        .expect("create aliased-tree mounted entry");
    assert_eq!(
        worker.run(
            "bind",
            &[
                TreeFiles::text(&alias_mount_source),
                TreeFiles::text(&alias_submount),
            ]
        ),
        "ok"
    );
    let excluded = files.root.join("excluded");
    let overlap = excluded.join("overlap");
    let second_root = files.outer.join("second-root");
    fs::create_dir_all(&overlap).expect("excluded overlap tree");
    fs::create_dir(&second_root).expect("second protected root");
    let exact_parent = files.outer.join("exact-parent");
    fs::create_dir(&exact_parent).expect("create exact parent");
    let exact = exact_parent.join("exact");
    let exact_exclusion = files.root.join("excluded-exact");
    let future_excluded_dir = files.root.join("future-cache");
    let nested_direct = excluded.join("direct");
    let overlap_file = overlap.join("protected");
    let delete_only_file = second_root.join("delete-only");
    let write_only = exact_parent.join("write-only");
    for (path, content) in [
        (&exact, b"exact".as_slice()),
        (&exact_exclusion, b"excluded".as_slice()),
        (&nested_direct, b"direct".as_slice()),
        (&overlap_file, b"overlap".as_slice()),
        (&delete_only_file, b"delete-only".as_slice()),
        (&write_only, b"write-only".as_slice()),
    ] {
        fs::write(path, content).expect("classifier fixture file");
    }
    let write_hardlink = files.outside.join("write-hardlink");
    fs::hard_link(&write_only, &write_hardlink).expect("pre-existing outside exact hardlink");
    let tree_entry = files.root.join("existing");
    let tree_hardlink = files.outside.join("tree-hardlink");
    fs::hard_link(&tree_entry, &tree_hardlink).expect("pre-existing outside tree hardlink");
    assert_eq!(
        worker.run("openfd", &[TreeFiles::text(&write_only)]),
        "ok",
        "the descriptor must predate policy activation"
    );

    let protected =
        |path: &std::path::Path, kind, delete, ordinary_write, exclusions| FileProtectionTarget {
            path: TreeFiles::text(path).into(),
            kind,
            protections: FileProtections {
                delete,
                ordinary_write,
            },
            exclusions,
        };
    let backend = ActPlaneBackend::open().expect("structured classifier runtime");
    // This stage only reports structured readiness through file_delete_guard; the
    // per-protection capability flags are reported by a separate stage.
    let capabilities = backend.health().expect("structured health").capabilities;
    assert!(capabilities.file_delete_guard);
    assert!(!capabilities.dynamic_mount_topology);
    assert!(!capabilities.complete_content_protection);
    let events = backend.subscribe(Uuid::new_v4(), SubscriberClass::Required);
    let binding_id = Uuid::new_v4();
    let binding = backend
        .apply(ApplyPolicy {
            binding_id,
            agent_id: "structured-kernel-test".into(),
            session_id: None,
            root_pid: worker.pid(),
            process_start_time: worker.start_time(),
            policy_id: "structured-classifier-kernel".into(),
            policy_revision: "1".into(),
            policy_dsl: String::new(),
            policy_mode: Some(PolicyMode::Enforce),
            file_protection_policy: Some(FileProtectionPolicy {
                version: FILE_PROTECTION_POLICY_VERSION,
                topology_mode: FileProtectionTopologyMode::StaticAtApply,
                targets: vec![
                    protected(
                        &exact,
                        FileProtectionTargetKind::ExactFile,
                        true,
                        false,
                        vec![],
                    ),
                    protected(
                        &files.root,
                        FileProtectionTargetKind::DirectoryTree,
                        true,
                        true,
                        vec![
                            FileProtectionExclusion {
                                path: TreeFiles::text(&exact_exclusion).into(),
                                kind: FileProtectionExclusionKind::ExactEntry,
                            },
                            FileProtectionExclusion {
                                path: TreeFiles::text(&excluded).into(),
                                kind: FileProtectionExclusionKind::DirectorySubtree,
                            },
                            FileProtectionExclusion {
                                path: TreeFiles::text(&future_excluded_dir).into(),
                                kind: FileProtectionExclusionKind::DirectorySubtree,
                            },
                        ],
                    ),
                    protected(
                        &authored_alias_tree,
                        FileProtectionTargetKind::DirectoryTree,
                        true,
                        true,
                        vec![],
                    ),
                    protected(
                        &nested_direct,
                        FileProtectionTargetKind::ExactFile,
                        true,
                        true,
                        vec![],
                    ),
                    protected(
                        &overlap,
                        FileProtectionTargetKind::DirectoryTree,
                        true,
                        false,
                        vec![],
                    ),
                    protected(
                        &second_root,
                        FileProtectionTargetKind::DirectoryTree,
                        true,
                        false,
                        vec![],
                    ),
                    protected(
                        &write_only,
                        FileProtectionTargetKind::ExactFile,
                        false,
                        true,
                        vec![],
                    ),
                ],
            }),
        })
        .expect("apply structured classifier policy");
    assert!(binding.domain_id.is_some());

    assert_eq!(worker.run("read", &[TreeFiles::text(&exact)]), "ok");
    assert_eq!(worker.run("write", &[TreeFiles::text(&exact)]), "ok");
    assert_eq!(worker.run("readfd", &[TreeFiles::text(&write_only)]), "ok");
    assert_eq!(
        worker.run("writefd", &[TreeFiles::text(&write_only)]),
        "errno=1"
    );
    assert_eq!(
        worker.run("pwritefd", &[TreeFiles::text(&write_only)]),
        "errno=1"
    );
    assert_eq!(worker.run("read", &[TreeFiles::text(&write_only)]), "ok");
    assert_eq!(
        worker.run("write", &[TreeFiles::text(&write_only)]),
        "errno=1"
    );
    assert_eq!(
        worker.run("pwrite", &[TreeFiles::text(&write_only)]),
        "errno=1"
    );
    for (protected_path, outside_link) in [
        (&write_only, &write_hardlink),
        (&tree_entry, &tree_hardlink),
    ] {
        let identity = fs::metadata(protected_path)
            .expect("protected hardlink inode")
            .ino();
        for operation in ["write", "pwrite"] {
            let before = fs::read(protected_path).expect("protected content before alias write");
            assert_eq!(
                worker.run(operation, &[TreeFiles::text(outside_link)]),
                "ok"
            );
            assert_eq!(
                fs::metadata(protected_path)
                    .expect("protected inode after write")
                    .ino(),
                identity
            );
            assert_ne!(
                fs::read(protected_path).expect("protected content after alias write"),
                before
            );
            assert_eq!(
                worker.run(operation, &[TreeFiles::text(protected_path)]),
                "errno=1"
            );
        }
        assert_eq!(worker.run("unlink", &[TreeFiles::text(outside_link)]), "ok");
        assert!(protected_path.exists());
    }
    for operation in ["write", "pwrite"] {
        assert_eq!(
            worker.run(operation, &[TreeFiles::text(&bind_alias.join("existing"))]),
            "errno=1"
        );
    }
    assert_eq!(
        worker.run(
            "unlink",
            &[TreeFiles::text(&external_mount_source.join("entry"))]
        ),
        "errno=1"
    );
    for operation in ["write", "pwrite", "unlink"] {
        assert_eq!(
            worker.run(
                operation,
                &[TreeFiles::text(&alias_mount_source.join("entry"))]
            ),
            "errno=1",
            "the backing source of a submount below an intermediate-symlink target must be registered"
        );
    }
    assert_eq!(worker.run("unlink", &[TreeFiles::text(&exact)]), "errno=1");
    let replacement = files.outside.join("replacement");
    fs::write(&replacement, b"replacement").expect("replacement source");
    assert_eq!(
        worker.run(
            "rename",
            &[TreeFiles::text(&replacement), TreeFiles::text(&exact)]
        ),
        "errno=1"
    );

    let created_empty = files.root.join("created-empty");
    assert_eq!(
        worker.run("create_empty", &[TreeFiles::text(&created_empty)]),
        "ok",
        "entry creation without initialization writes remains allowed"
    );
    assert_eq!(worker.run("read", &[TreeFiles::text(&created_empty)]), "ok");
    assert_eq!(
        worker.run("openfd", &[TreeFiles::text(&created_empty)]),
        "ok"
    );
    assert_eq!(
        worker.run("writefd", &[TreeFiles::text(&created_empty)]),
        "errno=1"
    );
    assert_eq!(
        worker.run("pwritefd", &[TreeFiles::text(&created_empty)]),
        "errno=1"
    );
    assert_eq!(
        worker.run("write", &[TreeFiles::text(&created_empty)]),
        "errno=1"
    );
    assert_eq!(
        worker.run("pwrite", &[TreeFiles::text(&created_empty)]),
        "errno=1"
    );
    let future = files.root.join("future");
    assert_eq!(
        worker.run("touch", &[TreeFiles::text(&future)]),
        "errno=1",
        "the create may succeed but its initialization write must not"
    );
    assert_eq!(worker.run("read", &[TreeFiles::text(&future)]), "ok");
    assert_eq!(worker.run("unlink", &[TreeFiles::text(&future)]), "errno=1");
    assert_eq!(
        worker.run("unlink", &[TreeFiles::text(&bind_alias.join("future"))]),
        "errno=1"
    );
    let empty = files.root.join("empty-structured");
    assert_eq!(worker.run("mkdir", &[TreeFiles::text(&empty)]), "ok");
    assert_eq!(worker.run("rmdir", &[TreeFiles::text(&empty)]), "errno=1");
    assert_eq!(
        worker.run("rmdir", &[TreeFiles::text(&files.root)]),
        "errno=1"
    );

    assert_eq!(
        worker.run("write", &[TreeFiles::text(&exact_exclusion)]),
        "ok"
    );
    assert_eq!(
        worker.run("pwrite", &[TreeFiles::text(&exact_exclusion)]),
        "ok"
    );
    assert_eq!(
        worker.run("unlink", &[TreeFiles::text(&exact_exclusion)]),
        "ok"
    );
    assert_eq!(
        worker.run("touch", &[TreeFiles::text(&exact_exclusion)]),
        "ok"
    );
    assert_eq!(
        worker.run("unlink", &[TreeFiles::text(&exact_exclusion)]),
        "ok"
    );
    let exclusion_replacement = files.outside.join("exclusion-replacement");
    fs::write(&exclusion_replacement, b"replacement").expect("exclusion replacement");
    assert_eq!(
        worker.run(
            "rename",
            &[
                TreeFiles::text(&exclusion_replacement),
                TreeFiles::text(&exact_exclusion),
            ]
        ),
        "ok"
    );
    let moved_exclusion = files.outside.join("moved-exclusion");
    assert_eq!(
        worker.run(
            "rename",
            &[
                TreeFiles::text(&exact_exclusion),
                TreeFiles::text(&moved_exclusion),
            ]
        ),
        "ok"
    );
    assert_eq!(
        worker.run(
            "rename",
            &[
                TreeFiles::text(&moved_exclusion),
                TreeFiles::text(&exact_exclusion),
            ]
        ),
        "ok"
    );
    let excluded_future = excluded.join("future");
    assert_eq!(
        worker.run("touch", &[TreeFiles::text(&excluded_future)]),
        "ok"
    );
    assert_eq!(
        worker.run("pwrite", &[TreeFiles::text(&excluded_future)]),
        "ok"
    );
    assert_eq!(
        worker.run("unlink", &[TreeFiles::text(&excluded_future)]),
        "ok"
    );
    assert_eq!(
        worker.run("mkdir", &[TreeFiles::text(&future_excluded_dir)]),
        "ok"
    );
    let future_excluded_child = future_excluded_dir.join("child");
    assert_eq!(
        worker.run("touch", &[TreeFiles::text(&future_excluded_child)]),
        "ok"
    );
    assert_eq!(
        worker.run("unlink", &[TreeFiles::text(&future_excluded_child)]),
        "ok"
    );
    assert_eq!(
        worker.run("rmdir", &[TreeFiles::text(&future_excluded_dir)]),
        "ok"
    );
    assert_eq!(worker.run("read", &[TreeFiles::text(&nested_direct)]), "ok");
    assert_eq!(
        worker.run("write", &[TreeFiles::text(&nested_direct)]),
        "errno=1",
        "an explicit write target must override its ancestor exclusion"
    );
    assert_eq!(
        worker.run("pwrite", &[TreeFiles::text(&nested_direct)]),
        "errno=1"
    );
    assert_eq!(
        worker.run("unlink", &[TreeFiles::text(&nested_direct)]),
        "errno=1"
    );
    assert_eq!(worker.run("write", &[TreeFiles::text(&overlap_file)]), "ok");
    assert_eq!(
        worker.run("unlink", &[TreeFiles::text(&overlap_file)]),
        "errno=1"
    );
    assert_eq!(
        worker.run("write", &[TreeFiles::text(&delete_only_file)]),
        "ok",
        "delete-only targets must permit ordinary writes"
    );
    assert_eq!(
        worker.run("pwrite", &[TreeFiles::text(&delete_only_file)]),
        "ok"
    );
    assert_eq!(
        worker.run("unlink", &[TreeFiles::text(&delete_only_file)]),
        "errno=1"
    );

    let cross = files.root.join("cross-root");
    fs::write(&cross, b"cross").expect("cross-root source");
    assert_eq!(
        worker.run(
            "rename",
            &[
                TreeFiles::text(&cross),
                TreeFiles::text(&second_root.join("cross-root"))
            ]
        ),
        "ok"
    );
    let protected_to_excluded = files.root.join("protected-to-excluded");
    fs::write(&protected_to_excluded, b"protected").expect("protected move source");
    assert_eq!(
        worker.run(
            "rename",
            &[
                TreeFiles::text(&protected_to_excluded),
                TreeFiles::text(&excluded.join("protected-to-excluded")),
            ]
        ),
        "errno=1"
    );
    let excluded_to_protected = excluded.join("excluded-to-protected");
    fs::write(&excluded_to_protected, b"excluded").expect("excluded move source");
    assert_eq!(
        worker.run(
            "rename",
            &[
                TreeFiles::text(&excluded_to_protected),
                TreeFiles::text(&files.root.join("excluded-to-protected")),
            ]
        ),
        "ok"
    );

    let observed: Vec<_> = events.try_iter().collect();
    assert!(
        observed
            .iter()
            .any(|event| event.blocked && event.rule_id.is_some()),
        "structured denials must retain a stable target rule id"
    );
    assert!(
        observed.iter().any(|event| {
            event.operation == "ordinary_write" && event.blocked && event.rule_id.is_some()
        }),
        "ordinary-write denials must identify their operation and target"
    );
    backend
        .detach(binding_id)
        .expect("detach structured binding");
    assert_eq!(worker.run("write", &[TreeFiles::text(&write_only)]), "ok");
    assert_eq!(worker.run("unlink", &[TreeFiles::text(&exact)]), "ok");
}

fn file_guard() {
    let files = Files::new("agent-file-guard");
    let source = files.source();
    let mut worker = Worker::spawn(&source);
    let backend = ActPlaneBackend::open().expect("file guard runtime");
    assert!(
        backend
            .health()
            .expect("health")
            .capabilities
            .file_delete_guard
    );
    let events = backend.subscribe(Uuid::new_v4(), SubscriberClass::Required);
    let binding_id = Uuid::new_v4();
    backend.apply(ApplyPolicy {
        binding_id,
        agent_id: "inode-test".into(),
        session_id: None,
        root_pid: worker.pid(),
        process_start_time: worker.start_time(),
        policy_id: "inode-guard".into(),
        policy_revision: "1".into(),
        policy_dsl: format!(
            "source AGENT = exec \"**\"\nrule unrelated:\nnotify open file \"/tmp/never-matches\" if AGENT\nbecause \"unrelated\"\nrule protected:\nblock unlink file \"{source}\" if AGENT\nbecause \"keep fixture\"\n"
        ),
        policy_mode: Some(PolicyMode::Enforce),
        file_protection_policy: None,
    }).expect("apply file guard");
    for operation in ["unlink", "rename"] {
        assert_eq!(worker.run(operation), "errno=1");
        assert!(PathBuf::from(&source).exists());
        let event = events
            .recv_timeout(Duration::from_secs(3))
            .expect("guard audit event");
        assert_eq!(event.operation, operation);
        assert_eq!(event.target, source);
        assert_eq!(event.rule_id.as_deref(), Some("protected"));
        assert_eq!(event.reason.as_deref(), Some("keep fixture"));
        assert!(event.blocked);
    }
    let unguarded_path = files.root.join("outside");
    fs::hard_link(&source, &unguarded_path).expect("outside-domain hardlink");
    fs::remove_file(&unguarded_path).expect("unbound caller is not blocked");
    backend.detach(binding_id).expect("detach guard");
    assert_eq!(worker.run("unlink"), "ok");
    assert!(events.recv_timeout(Duration::from_millis(250)).is_err());
}

fn receive_chain(events: &Receiver<SecurityEvent>, source: &str) {
    let mut found_source = false;
    let mut found_taint = false;
    let mut found_network = false;
    for _ in 0..12 {
        let event = events
            .recv_timeout(Duration::from_secs(3))
            .expect("credential security event");
        match event.kind {
            SecurityEventKind::FileAction(action) => {
                assert_eq!(action.path, source);
                assert_eq!(action.operation, "open");
                found_source = true;
            }
            SecurityEventKind::TaintTransition(_) => found_taint = true,
            SecurityEventKind::NetworkAction(_) => found_network = true,
            SecurityEventKind::EnforcementState(state) => {
                panic!("unexpected runtime state: {state:?}")
            }
            SecurityEventKind::PolicyDecision(decision) => {
                assert!(!decision.blocked);
                assert_eq!(decision.mode, PolicyMode::Audit);
                assert!(found_source && found_taint && found_network);
                return;
            }
        }
    }
    panic!("missing credential decision");
}

fn credentials(read_operation: &str) {
    let files = Files::new("credential-exfiltration");
    let source = files.source();
    let mut worker = Worker::spawn(&source);
    let backend = ActPlaneBackend::open().expect("credential runtime");
    let events = backend.subscribe_security_events();
    let binding_id = Uuid::new_v4();
    backend
        .apply_credential_policy(ApplyCredentialPolicy {
            binding_id,
            agent_id: "inode-test".into(),
            session_id: None,
            root_pid: worker.pid(),
            process_start_time: worker.start_time(),
            policy: CredentialExfiltrationPolicy {
                policy_id: "credential-fixture".into(),
                revision: 1,
                source_patterns: vec![source.clone()],
                trusted_endpoints: vec!["9.9.9.9".into()],
                taint_label: "CREDENTIAL".into(),
                taint_ttl_secs: 900,
                destination_scope: DestinationScope::PublicIpv4,
                mode: PolicyMode::Audit,
            },
        })
        .expect("apply credential policy");
    assert_eq!(worker.run("connect 8.8.8.8"), "ok");
    assert!(events.recv_timeout(Duration::from_millis(250)).is_err());
    {
        let mut unbound = Worker::spawn(&source);
        assert_eq!(unbound.run(read_operation), "ok");
        assert_eq!(unbound.run("connect 8.8.8.8"), "ok");
        assert!(events.recv_timeout(Duration::from_millis(250)).is_err());
    }
    assert_eq!(worker.run(read_operation), "ok");
    assert_eq!(worker.run("connect 9.9.9.9"), "ok");
    assert_eq!(worker.run("connect 127.0.0.1"), "ok");
    assert!(events.recv_timeout(Duration::from_millis(250)).is_err());
    assert_eq!(worker.run("connect 8.8.8.8"), "ok");
    receive_chain(&events, &source);
    backend
        .detach(binding_id)
        .expect("detach credential policy");
    while events.try_recv().is_ok() {}
    assert_eq!(worker.run("read"), "ok");
    assert_eq!(worker.run("connect 8.8.8.8"), "ok");
    assert!(events.recv_timeout(Duration::from_millis(250)).is_err());
    assert_eq!(worker.run("unlink"), "ok");
}

#[test]
#[ignore = "requires root, active BPF LSM, unlimited memlock, and --test-threads=1"]
fn inode_guard_and_credential_events_on_live_kernel() {
    file_guard();
    for read_operation in ["read", "readfd", "mmap"] {
        credentials(read_operation);
    }
}

#[test]
#[ignore = "requires root, active BPF LSM, unlimited memlock, and --test-threads=1"]
fn structured_policy_classifier_on_live_kernel() {
    structured_policy_classifier();
}
