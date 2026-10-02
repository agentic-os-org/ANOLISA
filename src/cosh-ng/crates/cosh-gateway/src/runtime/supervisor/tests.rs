//! Focused supervisor lifecycle and cleanup tests.

use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::tempdir;

use super::*;

#[cfg(target_os = "linux")]
fn linked_shell(directory: &Path, name: &str) -> std::path::PathBuf {
    let path = directory.join(name);
    std::os::unix::fs::symlink("/bin/sh", &path).unwrap();
    path
}

fn wait_for_terminal(supervisor: &mut RuntimeSupervisor) -> ProcessTerminal {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(terminal) = supervisor.poll_terminal().unwrap() {
            return terminal;
        }
        assert!(Instant::now() < deadline, "child did not exit");
        thread::sleep(Duration::from_millis(5));
    }
}

#[derive(Debug, Default)]
struct TermFailingProcessGroup {
    terminate_calls: AtomicUsize,
    kill_calls: AtomicUsize,
}

impl ProcessGroupLifecycle for TermFailingProcessGroup {
    fn configure(&self, command: &mut Command) {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
    }

    fn terminate(&self, _process_group: u32) -> io::Result<()> {
        self.terminate_calls.fetch_add(1, Ordering::SeqCst);
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "injected TERM failure",
        ))
    }

    fn kill(&self, _process_group: u32) -> io::Result<()> {
        self.kill_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn launch_validation_rejects_relative_program_before_state_change() {
    let workspace = tempdir().unwrap();
    let spec = RuntimeLaunchSpec::new("sh", workspace.path());
    let mut supervisor = RuntimeSupervisor::new();

    assert!(matches!(
        supervisor.launch(&spec),
        Err(RuntimeSupervisorError::Launch(
            RuntimeLaunchError::ProgramNotAbsolute(_)
        ))
    ));
    assert_eq!(supervisor.state(), RuntimeState::Idle);
}

#[cfg(unix)]
#[test]
fn reaps_once_and_retains_bounded_stderr_tail() {
    let workspace = tempdir().unwrap();
    let mut spec = RuntimeLaunchSpec::new("/bin/sh", workspace.path());
    spec.arguments = vec![
        "-c".into(),
        "printf 'ready\\n'; head -c 4096 /dev/zero | tr '\\000' x >&2; exit 7".into(),
    ];
    spec.stderr_capacity = 64;
    let mut supervisor = RuntimeSupervisor::new();

    supervisor.launch(&spec).unwrap();
    assert_eq!(supervisor.state(), RuntimeState::Initializing);
    supervisor.mark_ready().unwrap();
    assert_eq!(supervisor.read_frame().unwrap().as_deref(), Some("ready"));

    let deadline = Instant::now() + Duration::from_secs(2);
    let terminal = loop {
        if let Some(terminal) = supervisor.poll_terminal().unwrap() {
            break terminal;
        }
        assert!(Instant::now() < deadline, "child did not exit");
        thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(terminal.exit, ProcessExit::Code(7));
    assert_eq!(terminal.stderr.tail, "x".repeat(64));
    assert_eq!(terminal.stderr.discarded_bytes, 4032);
    assert_eq!(supervisor.poll_terminal().unwrap(), None);
}

#[cfg(unix)]
#[test]
fn shutdown_escalates_and_reaps_term_ignoring_child() {
    let workspace = tempdir().unwrap();
    let mut spec = RuntimeLaunchSpec::new("/bin/sh", workspace.path());
    spec.arguments = vec![
        "-c".into(),
        "trap '' TERM; printf 'ready\\n'; while :; do sleep 1; done".into(),
    ];
    let mut supervisor = RuntimeSupervisor::new();

    supervisor.launch(&spec).unwrap();
    assert_eq!(supervisor.read_frame().unwrap().as_deref(), Some("ready"));
    let terminal = supervisor
        .shutdown(Duration::from_millis(20))
        .unwrap()
        .unwrap();

    assert_eq!(terminal.exit, ProcessExit::Signal(9));
    assert_eq!(supervisor.state(), RuntimeState::Exited);
    assert_eq!(supervisor.poll_terminal().unwrap(), None);
}

#[cfg(unix)]
#[test]
fn stdin_write_deadline_keeps_shutdown_available() {
    let workspace = tempdir().unwrap();
    let mut spec = RuntimeLaunchSpec::new("/bin/sh", workspace.path());
    spec.arguments = vec!["-c".into(), "sleep 60".into()];
    spec.stdin_write_timeout = Duration::from_millis(30);
    let mut supervisor = RuntimeSupervisor::new();
    supervisor.launch(&spec).unwrap();

    let frame = "x".repeat(256 * 1024);
    assert!(matches!(
        supervisor.write_frame(&frame),
        Err(RuntimeSupervisorError::Process(ref error))
            if error.kind() == io::ErrorKind::TimedOut
    ));
    assert!(supervisor
        .shutdown(Duration::from_millis(30))
        .unwrap()
        .is_some());
    assert_eq!(supervisor.state(), RuntimeState::Exited);
}

#[cfg(unix)]
#[test]
fn term_group_failure_still_kills_reaps_and_settles_once() {
    let workspace = tempdir().unwrap();
    let mut spec = RuntimeLaunchSpec::new("/bin/sh", workspace.path());
    spec.arguments = vec!["-c".into(), "printf 'ready\\n'; while :; do :; done".into()];
    let process_group = Arc::new(TermFailingProcessGroup::default());
    let mut supervisor = RuntimeSupervisor::with_process_group(process_group.clone());

    supervisor.launch(&spec).unwrap();
    assert_eq!(supervisor.read_frame().unwrap().as_deref(), Some("ready"));
    let error = supervisor.shutdown(Duration::from_secs(1)).unwrap_err();

    assert!(matches!(
        error,
        RuntimeSupervisorError::ProcessGroupSignal { signal: "TERM", .. }
    ));
    assert_eq!(process_group.terminate_calls.load(Ordering::SeqCst), 1);
    assert_eq!(supervisor.state(), RuntimeState::Exited);
    let terminal = supervisor.poll_terminal().unwrap().unwrap();
    assert_eq!(terminal.exit, ProcessExit::Signal(9));
    assert_eq!(supervisor.poll_terminal().unwrap(), None);
}

#[cfg(target_os = "linux")]
#[test]
fn executable_and_workspace_replacements_do_not_redirect_launch() {
    let root = tempdir().unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let executable = linked_shell(root.path(), "runtime");
    let mut spec = RuntimeLaunchSpec::new(&executable, &workspace);
    spec.arguments = vec!["-c".into(), "printf original > launch.marker".into()];

    let admitted_workspace = root.path().join("admitted-workspace");
    fs::rename(&workspace, &admitted_workspace).unwrap();
    fs::create_dir(&workspace).unwrap();
    let admitted_executable = root.path().join("admitted-runtime");
    fs::rename(&executable, &admitted_executable).unwrap();
    fs::copy("/bin/false", &executable).unwrap();

    let mut supervisor = RuntimeSupervisor::new();
    supervisor.launch(&spec).unwrap();
    assert_eq!(
        wait_for_terminal(&mut supervisor).exit,
        ProcessExit::Code(0)
    );
    assert_eq!(
        fs::read_to_string(admitted_workspace.join("launch.marker")).unwrap(),
        "original"
    );
    assert!(!workspace.join("launch.marker").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn pinned_launch_is_stable_across_multiple_runs() {
    let root = tempdir().unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let executable = linked_shell(root.path(), "runtime");
    let mut spec = RuntimeLaunchSpec::new(&executable, &workspace);
    spec.arguments = vec![
        "-c".into(),
        "printf x >> multi-run.marker; printf 'ready\\n'".into(),
    ];

    let mut first = RuntimeSupervisor::new();
    first.launch(&spec).unwrap();
    assert_eq!(first.read_frame().unwrap().as_deref(), Some("ready"));
    assert_eq!(wait_for_terminal(&mut first).exit, ProcessExit::Code(0));

    let admitted_workspace = root.path().join("admitted-workspace");
    fs::rename(&workspace, &admitted_workspace).unwrap();
    fs::create_dir(&workspace).unwrap();
    fs::rename(&executable, root.path().join("admitted-runtime")).unwrap();
    fs::copy("/bin/false", &executable).unwrap();

    let mut second = RuntimeSupervisor::new();
    second.launch(&spec).unwrap();
    assert_eq!(second.read_frame().unwrap().as_deref(), Some("ready"));
    assert_eq!(wait_for_terminal(&mut second).exit, ProcessExit::Code(0));
    assert_eq!(
        fs::read_to_string(admitted_workspace.join("multi-run.marker")).unwrap(),
        "xx"
    );
    assert!(!workspace.join("multi-run.marker").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn pinned_descriptors_close_after_specs_are_dropped() {
    for _ in 0..16 {
        let root = tempdir().unwrap();
        let executable = linked_shell(root.path(), "runtime");
        let mut spec = RuntimeLaunchSpec::new(executable, root.path());
        let (program, directory) = match (&spec.program, &spec.working_directory) {
            (LaunchProgram::Pinned { executable, .. }, LaunchDirectory::Pinned(directory)) => {
                (executable.descriptor_weak(), directory.descriptor_weak())
            }
            _ => panic!("expected pinned launch handles"),
        };
        spec.arguments = vec!["-c".into(), "exit 0".into()];
        let mut supervisor = RuntimeSupervisor::new();
        supervisor.launch(&spec).unwrap();
        assert_eq!(
            wait_for_terminal(&mut supervisor).exit,
            ProcessExit::Code(0)
        );
        drop(supervisor);
        drop(spec);
        assert!(program.upgrade().is_none());
        assert!(directory.upgrade().is_none());
    }
}
