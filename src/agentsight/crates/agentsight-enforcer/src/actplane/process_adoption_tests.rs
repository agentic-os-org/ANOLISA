//! 无特权的进程调度与 map 故障回归，不加载主机 BPF 策略。

use std::cell::{Cell, RefCell};

use super::*;

const ROOT: i32 = 100;
const DOMAIN: u32 = 7;

#[derive(Default)]
struct Fixture {
    processes: RefCell<Vec<Process>>,
    domains: RefCell<HashMap<i32, u32>>,
    trees: Cell<usize>,
    writes: Cell<usize>,
    revoked: Cell<usize>,
    fork_on_adopt: bool,
    final_child: bool,
    fail_map: bool,
    fail_query: bool,
    fail_tree: bool,
    reparent: bool,
    reuse_pid: bool,
    fail_revoke: bool,
    namespace_collision: bool,
    leave_tree_on_check: bool,
    departing_child_exits: bool,
    departing_pid_reused: bool,
}

impl Fixture {
    fn new(children: &[(i32, i32)]) -> Self {
        let mut processes = vec![Process {
            pid: ROOT,
            parent: 1,
            start: 10,
        }];
        processes.extend(children.iter().map(|&(pid, parent)| Process {
            pid,
            parent,
            start: pid as u64,
        }));
        Self {
            processes: RefCell::new(processes),
            domains: RefCell::new(HashMap::from([(ROOT, DOMAIN)])),
            ..Self::default()
        }
    }
    fn run(&self) -> Report {
        adopt(self, ROOT, 10, ROOT, std::process::id() as i32, DOMAIN)
    }
}

impl Surface for Fixture {
    fn validate_namespace(&self) -> io::Result<()> {
        if self.namespace_collision {
            Err(io::Error::other(
                "kernel init PID is not 1 despite root/control PID collision",
            ))
        } else {
            Ok(())
        }
    }
    fn tree(&self, _: i32) -> io::Result<Vec<Process>> {
        self.trees.set(self.trees.get() + 1);
        if self.fail_tree {
            return Err(io::Error::other("procfs unavailable"));
        }
        if self.final_child && self.trees.get() == 2 {
            self.processes.borrow_mut().push(Process {
                pid: 200,
                parent: ROOT,
                start: 200,
            });
        }
        let mut reachable = HashSet::from([ROOT]);
        let mut tree = Vec::new();
        for task in self.processes.borrow().iter() {
            if task.pid == ROOT || reachable.contains(&task.parent) {
                reachable.insert(task.pid);
                tree.push(*task);
            }
        }
        Ok(tree)
    }
    fn process(&self, pid: i32) -> io::Result<Option<Process>> {
        if self.leave_tree_on_check && pid == 300 {
            let mut tasks = self.processes.borrow_mut();
            if tasks.iter().any(|task| task.pid == 200) {
                tasks.retain(|task| {
                    task.pid != 200 && !(self.departing_child_exits && task.pid == 300)
                });
                if let Some(child) = tasks.iter_mut().find(|task| task.pid == 300) {
                    child.parent = 1;
                    child.start += u64::from(self.departing_pid_reused);
                }
            }
        }
        Ok(self
            .processes
            .borrow()
            .iter()
            .find(|process| process.pid == pid)
            .copied())
    }
    fn domain(&self, pid: i32) -> io::Result<Option<u32>> {
        if self.fail_query && pid != ROOT {
            return Err(io::Error::other("map lookup failed"));
        }
        Ok(self.domains.borrow().get(&pid).copied())
    }
    fn adopt(&self, parent: i32, child: i32, domain: u32) -> io::Result<bool> {
        if self.domains.borrow().contains_key(&child) || self.domain(parent)? != Some(domain) {
            return Ok(false);
        }
        self.writes.set(self.writes.get() + 1);
        if self.fail_map {
            return Err(io::Error::other("map write failed"));
        }
        if self.fork_on_adopt {
            // fork 发生在写 cap_task 之前，模拟孩子不继承策略域。
            self.processes.borrow_mut().push(Process {
                pid: child + 1,
                parent: child,
                start: (child + 1) as u64,
            });
        }
        self.domains.borrow_mut().insert(child, domain);
        if self.reparent || self.reuse_pid {
            let mut processes = self.processes.borrow_mut();
            let task = processes.iter_mut().find(|task| task.pid == child).unwrap();
            if self.reparent {
                task.parent = ROOT;
            }
            if self.reuse_pid {
                task.start += 1;
            }
        }
        Ok(true)
    }
    fn revoke(&self, pid: i32, _: u32) -> io::Result<()> {
        self.revoked.set(self.revoked.get() + 1);
        if self.fail_revoke {
            return Err(io::Error::other("revocation failed"));
        }
        self.domains.borrow_mut().remove(&pid);
        Ok(())
    }
}

#[test]
fn stable_descendants_are_adopted_parents_first() {
    let fixture = Fixture::new(&[(200, ROOT), (300, 200)]);
    let report = fixture.run();
    assert_eq!(report.gaps, 0, "{:?}", report.details);
    assert_eq!(report.adopted, 2);
    assert_eq!(fixture.domain(300).unwrap(), Some(DOMAIN));
}

#[test]
fn forks_racing_every_pass_are_reported_without_a_fifth_adoption() {
    let fixture = Fixture {
        fork_on_adopt: true,
        ..Fixture::new(&[(200, ROOT)])
    };
    let report = fixture.run();
    assert_eq!(fixture.writes.get(), MAX_PASSES);
    assert_eq!(fixture.trees.get(), MAX_PASSES + 1);
    assert!(report.details.iter().any(|gap| gap.contains("pid 204")));
    assert_eq!(fixture.domain(204).unwrap(), None);
}

#[test]
fn final_snapshot_is_strictly_read_only_even_when_adoption_would_succeed() {
    let fixture = Fixture {
        final_child: true,
        ..Fixture::new(&[])
    };
    let report = fixture.run();
    assert_eq!(fixture.writes.get(), 0);
    assert_eq!(report.gaps, 1);
    assert!(report.details[0].contains("pid 200"));
}

#[test]
fn final_map_lookup_errors_are_not_filtered_out() {
    let fixture = Fixture {
        final_child: true,
        fail_query: true,
        ..Fixture::new(&[])
    };
    let report = fixture.run();
    assert_eq!(report.gaps, 1);
    assert!(report.details[0].contains("map lookup failed"));
}

#[test]
fn map_write_failure_and_procfs_failure_are_visible() {
    let fixture = Fixture {
        fail_map: true,
        ..Fixture::new(&[(200, ROOT)])
    };
    let report = fixture.run();
    assert!(
        report
            .details
            .iter()
            .any(|gap| gap.contains("map write failed"))
    );
    assert!(
        report
            .details
            .iter()
            .any(|gap| gap.contains("pid 200 is outside"))
    );
    let fixture = Fixture {
        fail_tree: true,
        ..Fixture::new(&[])
    };
    assert!(fixture.run().gaps > 0);
}

#[test]
fn subreaper_reparenting_does_not_revoke_the_same_process() {
    let fixture = Fixture {
        reparent: true,
        ..Fixture::new(&[(200, ROOT), (300, 200)])
    };
    fixture.domains.borrow_mut().insert(200, DOMAIN);
    let report = fixture.run();
    assert_eq!(report.gaps, 0);
    assert_eq!(fixture.process(300).unwrap().unwrap().parent, ROOT);
    assert_eq!(fixture.domain(300).unwrap(), Some(DOMAIN));
    assert_eq!(fixture.revoked.get(), 0);
}

#[test]
fn surviving_descendant_reparented_outside_tree_is_not_lost() {
    let fixture = Fixture {
        leave_tree_on_check: true,
        ..Fixture::new(&[(200, ROOT), (300, 200)])
    };
    fixture.domains.borrow_mut().insert(200, DOMAIN);
    let report = fixture.run();
    assert!(
        !fixture
            .tree(ROOT)
            .unwrap()
            .iter()
            .any(|task| task.pid == 300)
    );
    assert!(fixture.process(300).unwrap().is_some());
    assert_eq!(fixture.domain(300).unwrap(), None);
    assert!(
        report
            .details
            .iter()
            .any(|gap| gap.contains("pid 300 is outside")),
        "{:?}",
        report.details
    );
    assert_eq!(fixture.writes.get(), 0);
}

#[test]
fn departed_exit_or_reused_pid_is_not_confused_with_live_descendant() {
    for (exits, reused, covered) in [
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        let fixture = Fixture {
            leave_tree_on_check: true,
            departing_child_exits: exits,
            departing_pid_reused: reused,
            ..Fixture::new(&[(200, ROOT), (300, 200)])
        };
        fixture.domains.borrow_mut().insert(200, DOMAIN);
        if covered {
            fixture.domains.borrow_mut().insert(300, DOMAIN);
        }
        assert_eq!(fixture.run().gaps, 0);
        assert_eq!(fixture.writes.get(), 0);
    }
}

#[test]
fn observed_identities_have_one_bound_across_all_passes() {
    let mut seen = HashMap::new();
    for start in 0..MAX_PROCESSES {
        assert!(remember(
            &mut seen,
            &[Process {
                pid: 1,
                parent: 0,
                start: start as u64
            }]
        ));
    }
    assert!(!remember(
        &mut seen,
        &[Process {
            pid: 1,
            parent: 0,
            start: MAX_PROCESSES as u64
        }]
    ));
    assert_eq!(seen.len(), MAX_PROCESSES);
}

#[test]
fn reused_pid_is_revoked_and_failed_revocation_is_reported() {
    for fail_revoke in [false, true] {
        let fixture = Fixture {
            reuse_pid: true,
            fail_revoke,
            ..Fixture::new(&[(200, ROOT)])
        };
        let report = fixture.run();
        assert!(report.gaps > 0);
        assert!(fixture.revoked.get() > 0);
        if fail_revoke {
            assert!(
                report
                    .details
                    .iter()
                    .any(|gap| gap.contains("revocation failed"))
            );
        }
    }
}

#[test]
fn namespace_mismatch_is_degraded_without_map_or_procfs_calls() {
    let fixture = Fixture::new(&[]);
    let report = adopt(
        &fixture,
        ROOT,
        10,
        ROOT + 1,
        std::process::id() as i32,
        DOMAIN,
    );
    assert_eq!(report.gaps, 1);
    assert_eq!(fixture.trees.get(), 0);
    assert_eq!(fixture.writes.get(), 0);
}

#[test]
fn equal_pid_numbers_do_not_override_namespace_identity_failure() {
    let fixture = Fixture {
        namespace_collision: true,
        ..Fixture::new(&[(200, ROOT)])
    };
    let report = fixture.run();
    assert_eq!(report.gaps, 1);
    assert_eq!(fixture.trees.get(), 0);
    assert_eq!(fixture.writes.get(), 0);
    assert!(report.details[0].contains("PID namespace"));
}

#[test]
#[ignore = "requires root and BPF iterator; run host and unshare --pid --fork --mount-proc"]
fn live_initial_namespace_guard_rejects_container_procfs() {
    let expected = std::env::var("ACTPLANE_EXPECT_INITIAL_NAMESPACE").unwrap();
    let result = validate_initial_procfs();
    if expected == "yes" {
        assert!(result.is_ok(), "{result:?}");
    } else {
        assert!(
            result.is_err(),
            "container-local procfs must never be treated as kernel PID keys"
        );
    }
}

#[test]
fn reused_root_is_never_used_to_adopt_children() {
    let fixture = Fixture::new(&[(200, ROOT)]);
    fixture.processes.borrow_mut()[0].start += 1;
    assert!(fixture.run().gaps > 0);
    assert_eq!(fixture.writes.get(), 0);
}

#[test]
fn another_domain_is_not_claimed_as_covered() {
    let fixture = Fixture::new(&[(200, ROOT)]);
    fixture.domains.borrow_mut().insert(200, DOMAIN + 1);
    assert!(fixture.run().gaps > 0);
    assert_eq!(fixture.domain(200).unwrap(), Some(DOMAIN + 1));
}

#[test]
fn coverage_gap_survives_binding_serialization_with_migration_message() {
    use agentsight_enforcement_protocol::{ApplyPolicy, PolicyMode};
    let fixture = Fixture {
        final_child: true,
        ..Fixture::new(&[])
    };
    let mut binding = Binding {
        request: ApplyPolicy {
            binding_id: uuid::Uuid::new_v4(),
            agent_id: "fixture".into(),
            session_id: None,
            root_pid: ROOT,
            process_start_time: 10,
            policy_id: "fixture".into(),
            policy_revision: "1".into(),
            policy_dsl: "fixture".into(),
            policy_mode: Some(PolicyMode::Enforce),
            file_protection_policy: None,
        },
        state: BindingState::Enforced,
        message: Some("migration summary".into()),
        domain_id: Some(DOMAIN),
    };
    fixture.run().annotate(&mut binding);
    let wire = serde_json::to_vec(&binding).unwrap();
    let decoded: Binding = serde_json::from_slice(&wire).unwrap();
    assert_eq!(decoded.state, BindingState::Degraded);
    let message = decoded.message.unwrap();
    assert!(message.starts_with("migration summary;"));
    assert!(message.contains("pid 200"));
    assert_eq!(decoded.domain_id, Some(DOMAIN));
}

#[test]
fn diagnostics_remain_bounded() {
    let mut report = Report::default();
    for pid in 0..1000 {
        report.gap(format!("pid {pid}"));
    }
    assert_eq!(report.gaps, 1000);
    assert_eq!(report.details.len(), MAX_DETAILS);
}

#[test]
fn real_procfs_subreaper_keeps_adopted_survivor() {
    real_procfs_reparent(true);
}

#[test]
fn real_procfs_external_reparent_reports_uncovered_survivor() {
    real_procfs_reparent(false);
}

fn real_procfs_reparent(inside: bool) {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Child, Command, Stdio};
    use std::time::Duration;

    struct Helper(Child);
    impl Drop for Helper {
        fn drop(&mut self) {
            if let Some(input) = self.0.stdin.as_mut() {
                let _ = writeln!(input, "stop");
            }
            let _ = self.0.wait();
        }
    }
    let script = r#"
import ctypes, json, os, signal, sys
if int(sys.argv[1]): assert ctypes.CDLL(None).prctl(36, 1, 0, 0, 0) == 0
assert ctypes.CDLL(None).prctl(15, ctypes.c_char_p(b'bad-\xff)comm'), 0, 0, 0) == 0
r, w = os.pipe()
p = os.fork()
if p == 0:
    c = os.fork()
    if c == 0:
        while True: signal.pause()
    os.write(w, str(c).encode())
    while True: signal.pause()
c = int(os.read(r, 32))
print(json.dumps([os.getpid(), p, c]), flush=True)
sys.stdin.readline()
for pid in (p, c):
    try: os.kill(pid, signal.SIGTERM)
    except ProcessLookupError: pass
for pid in (p, c):
    try: os.waitpid(pid, 0)
    except ChildProcessError: pass
"#;
    let mut helper = Helper(
        Command::new("python3")
            .args(["-u", "-c", script, if inside { "1" } else { "0" }])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    BufReader::new(helper.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let pids: Vec<i32> = serde_json::from_str(&line).unwrap();
    struct Live {
        root: i32,
        parent: i32,
        child: i32,
        inside: bool,
        domains: RefCell<HashMap<i32, u32>>,
    }
    impl Surface for Live {
        fn validate_namespace(&self) -> io::Result<()> {
            Ok(())
        }
        fn tree(&self, root: i32) -> io::Result<Vec<Process>> {
            process_tree(root)
        }
        fn process(&self, pid: i32) -> io::Result<Option<Process>> {
            if !self.inside
                && pid == self.child
                && read_process(pid)?.is_some_and(|task| task.parent == self.parent)
            {
                assert_eq!(unsafe { libc::kill(self.parent, libc::SIGTERM) }, 0);
                for _ in 0..100 {
                    if read_process(pid)?.is_some_and(|task| task.parent != self.parent) {
                        return read_process(pid);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                return Err(io::Error::other("external reparent timed out"));
            }
            read_process(pid)
        }
        fn domain(&self, pid: i32) -> io::Result<Option<u32>> {
            Ok(self.domains.borrow().get(&pid).copied())
        }
        fn adopt(&self, _: i32, child: i32, domain: u32) -> io::Result<bool> {
            if self.domains.borrow().contains_key(&child) {
                return Ok(false);
            }
            self.domains.borrow_mut().insert(child, domain);
            if child == self.child {
                // 父进程是专用 helper 创建且一直等待的子进程，不会自然退出或被复用。
                assert_eq!(unsafe { libc::kill(self.parent, libc::SIGTERM) }, 0);
                for _ in 0..100 {
                    if read_process(child)?.is_some_and(|task| task.parent == self.root) {
                        return Ok(true);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                return Err(io::Error::other("subreaper reparent timed out"));
            }
            Ok(true)
        }
        fn revoke(&self, _: i32, _: u32) -> io::Result<()> {
            panic!("same live process must not be revoked")
        }
    }
    let live = Live {
        root: pids[0],
        parent: pids[1],
        child: pids[2],
        inside,
        domains: RefCell::new(HashMap::from([(pids[0], DOMAIN), (pids[1], DOMAIN)])),
    };
    assert!(fs::read_to_string(format!("/proc/{}/stat", live.root)).is_err());
    let before = read_process(live.child).unwrap().unwrap();
    assert_eq!(before.parent, live.parent);
    let tree = process_tree(live.root).unwrap();
    let mut parents = HashSet::new();
    for task in tree {
        assert!(task.pid == live.root || parents.contains(&task.parent));
        parents.insert(task.pid);
    }
    let report = adopt(
        &live,
        live.root,
        read_process(live.root).unwrap().unwrap().start,
        live.root,
        std::process::id() as i32,
        DOMAIN,
    );
    let after = read_process(live.child).unwrap().unwrap();
    assert_eq!(after.start, before.start);
    if inside {
        assert_eq!(after.parent, live.root);
        assert_eq!(report.gaps, 0, "{:?}", report.details);
        assert_eq!(live.domain(live.child).unwrap(), Some(DOMAIN));
    } else {
        assert_ne!(after.parent, live.root);
        assert!(
            report
                .details
                .iter()
                .any(|gap| gap.contains(&format!("pid {} is outside", live.child))),
            "{:?}",
            report.details
        );
        assert_eq!(live.domain(live.child).unwrap(), None);
        assert!(
            !live
                .tree(live.root)
                .unwrap()
                .iter()
                .any(|task| task.pid == live.child)
        );
    }
}
