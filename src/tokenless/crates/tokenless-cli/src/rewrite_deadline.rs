//! Bounds the in-process RTK rewrite and ends the processes it started.
//!
//! RTK resolves the project root with `git rev-parse` and no deadline. Without
//! this bound a stalled `git` would hold PreTool until the caller gives up, and
//! would outlive a caller that stops only this process.
//!
//! On Unix the rewrite runs in a child of `compress` that leads a new process
//! group, and the deadline ends that group. Callers end only `compress`, so the
//! child's deadline covers its whole run. `compress` cannot use a group of its
//! own: when it already leads one, as the first command of a job-control
//! pipeline does, it cannot start another, and that group holds the rest of the
//! pipeline. On Windows the rewrite stays in `compress`, inside a job that holds
//! only this process and what it starts, and that ends with this process.
//! Without that group or job no rewrite runs, as a timeout could not end what
//! RTK started.

use std::io;
use std::process;
use std::sync::{Arc, Condvar, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::Duration;

/// The hidden `compress` flag that marks the rewrite child.
pub(crate) const CHILD_FLAG: &str = "rewrite-child";
/// The limit Tokenless applied while RTK ran as a separate process.
const REWRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a stderr write may take before the run goes on without it.
const REPORT_TIMEOUT: Duration = Duration::from_millis(200);

/// Runs the PreTool `request` through `tokenless compress` in a child that
/// leads a new process group, and relays the child's output.
///
/// This process stays in its caller's group, so the caller's signals and
/// terminal job control reach it as usual. The child bounds its whole run
/// itself and ends its group at the deadline, even after this process is gone.
///
/// Returns the status to exit with; the child has reported a non-zero one.
///
/// # Errors
///
/// Returns why the child could not run, or how it ended without a report.
#[cfg(unix)]
pub(crate) fn run_in_child(request: &str) -> Result<i32, String> {
    use std::io::{Read as _, Write as _};
    use std::os::unix::process::ExitStatusExt as _;
    use std::time::Instant;

    let started = Instant::now();
    let mut child =
        spawn_child().map_err(|error| format!("cannot start the RTK rewrite: {error}"))?;
    // A child that ends before it reads the request reports why itself.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(request.as_bytes());
    }
    // Piped rather than inherited: a background group that writes to a
    // terminal can be stopped, and a stopped child never reaches its deadline.
    let relay = match child.stderr.take().map(|mut stderr| {
        Reporter::spawn("rtk-rewrite-stderr", move || {
            let _ = io::copy(&mut stderr, &mut io::stderr());
        })
    }) {
        Some(Err(error)) => {
            // SAFETY: the child leads its own group, which nothing else joins.
            unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
            let _ = child.wait();
            return Err(format!("cannot relay the RTK rewrite's stderr: {error}"));
        }
        relay => relay.and_then(Result::ok),
    };
    let mut response = Vec::new();
    let read = child
        .stdout
        .take()
        .map_or(Ok(0), |mut stdout| stdout.read_to_end(&mut response));
    let status = child
        .wait()
        .map_err(|error| format!("cannot wait for the RTK rewrite: {error}"))?;
    if let Some(relay) = relay {
        relay.wait();
    }
    read.map_err(|error| format!("cannot read the RTK rewrite's response: {error}"))?;
    match (status.code(), status.signal()) {
        (Some(0), _) => {
            let mut stdout = io::stdout().lock();
            crate::stdout_result(stdout.write_all(&response).and_then(|()| stdout.flush()))
                .map_err(|(message, _)| message)?;
            Ok(0)
        }
        (Some(code), _) => Ok(code),
        // The deadline reports the timeout before it kills the child's group.
        (None, Some(libc::SIGKILL)) if started.elapsed() >= REWRITE_TIMEOUT => Ok(1),
        (_, signal) => Err(format!(
            "the RTK rewrite ended by signal {}",
            signal.unwrap_or_default()
        )),
    }
}

#[cfg(unix)]
fn spawn_child() -> io::Result<process::Child> {
    use std::os::unix::process::CommandExt as _;
    use std::path::Path;
    use std::process::{Command, Stdio};

    let child = |program: &Path| {
        Command::new(program)
            .args(["compress", &format!("--{CHILD_FLAG}")])
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    };
    // Still this binary after an upgrade replaced or removed its file; the
    // child then passes the command through, as it cannot launch RTK later.
    #[cfg(target_os = "linux")]
    if let Ok(started) = child(Path::new("/proc/self/exe")) {
        return Ok(started);
    }
    child(&std::env::current_exe()?)
}

/// Ends this process and everything the rewrite started unless dropped in time.
///
/// Once dropped, the process is never ended. `compress` drops it as soon as the
/// rewrite returns, so a response that is being written is never cut short; the
/// rewrite child holds it until it exits.
pub(crate) struct Deadline {
    finished: Arc<(Mutex<bool>, Condvar)>,
}

/// Starts the deadline; call it before the rewrite, and run no rewrite if it
/// fails.
///
/// # Errors
///
/// Returns why this process cannot own the processes the rewrite starts, or
/// the error from starting the watchdog thread.
pub(crate) fn start() -> io::Result<Deadline> {
    let tree = tree::Tree::own()?;
    let finished = Arc::new((Mutex::new(false), Condvar::new()));
    let watched = Arc::clone(&finished);
    thread::Builder::new()
        .name("rtk-rewrite-deadline".into())
        .spawn(move || watch(&watched, &tree))?;
    Ok(Deadline { finished })
}

impl Drop for Deadline {
    fn drop(&mut self) {
        let (finished, wake) = &*self.finished;
        *finished.lock().unwrap_or_else(PoisonError::into_inner) = true;
        wake.notify_one();
    }
}

fn watch(finished: &(Mutex<bool>, Condvar), tree: &tree::Tree) {
    let (lock, wake) = finished;
    let guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
    let (guard, _) = wake
        .wait_timeout_while(guard, REWRITE_TIMEOUT, |finished| !*finished)
        .unwrap_or_else(PoisonError::into_inner);
    if *guard {
        return;
    }
    // The guard stays held until the process ends, so dropping the deadline
    // blocks and no response follows the timeout report.
    let message = format!(
        "RTK rewrite timed out after {} seconds",
        REWRITE_TIMEOUT.as_secs()
    );
    // A write to a stderr pipe that nobody drains blocks, and so does taking
    // the stderr lock while the rewrite is stuck in such a write. Neither may
    // keep the processes alive.
    if let Ok(reporter) =
        Reporter::spawn("rtk-rewrite-report", move || crate::report_fatal(&message))
    {
        reporter.wait();
    }
    tree.kill();
    process::exit(1);
}

/// A thread that writes to stderr, which the run waits for only briefly.
struct Reporter {
    finished: mpsc::Receiver<()>,
}

impl Reporter {
    fn spawn(name: &str, report: impl FnOnce() + Send + 'static) -> io::Result<Self> {
        let (sender, finished) = mpsc::channel();
        thread::Builder::new().name(name.into()).spawn(move || {
            // The sender lives as long as the report runs.
            let _sender: mpsc::Sender<()> = sender;
            report();
        })?;
        Ok(Self { finished })
    }

    /// Returns once the report is done, or after [`REPORT_TIMEOUT`].
    fn wait(self) {
        // The thread never sends; its sender dropping ends the wait at once.
        let _ = self.finished.recv_timeout(REPORT_TIMEOUT);
    }
}

#[cfg(unix)]
mod tree {
    use std::io;

    /// This process leading its own group, which processes RTK starts inherit.
    pub(super) struct Tree;

    impl Tree {
        pub(super) fn own() -> io::Result<Self> {
            // `compress` starts the rewrite child as the leader of a new group,
            // so a group this process leads holds nothing of the caller's. A
            // process that leads no group was not started that way, and its
            // group holds the caller.
            // SAFETY: getpgrp and getpid take no arguments and cannot fail.
            if unsafe { libc::getpgrp() == libc::getpid() } {
                Ok(Self)
            } else {
                Err(io::Error::other(format!(
                    "`compress --{}` runs only as the leader of a new process group",
                    super::CHILD_FLAG
                )))
            }
        }

        /// Kills the group with this process in it. Sparing this process would
        /// mean leaving the group first, and a process that RTK forked in the
        /// meantime would then escape the signal.
        pub(super) fn kill(&self) {
            // Never signal a group this process does not lead.
            // SAFETY: getpgrp and getpid take no arguments and cannot fail.
            if unsafe { libc::getpgrp() == libc::getpid() } {
                // SAFETY: process group 0 is the group of this process, which it leads.
                unsafe { libc::killpg(0, libc::SIGKILL) };
            }
        }
    }
}

#[cfg(windows)]
mod tree {
    use std::io;
    use std::mem::ManuallyDrop;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
    use std::ptr::null;

    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    /// A job holding this process, which processes RTK starts join as well.
    ///
    /// A process cannot leave a job. Its one limit ends what is left in it when
    /// the last handle to it closes, so a caller that ends only this process
    /// also ends what RTK started.
    pub(super) struct Tree {
        /// Never closed before this process exits: closing it ends the job, this
        /// process included, even after the deadline is dropped.
        job: ManuallyDrop<OwnedHandle>,
    }

    impl Tree {
        pub(super) fn own() -> io::Result<Self> {
            // SAFETY: null attributes and name create a private, non-inheritable job.
            let raw = unsafe { CreateJobObjectW(null(), null()) };
            if raw.is_null() {
                let error = io::Error::last_os_error();
                return Err(io::Error::other(format!(
                    "cannot create a job object: {error}"
                )));
            }
            // SAFETY: the new handle is owned here alone.
            let job = unsafe { OwnedHandle::from_raw_handle(raw) };
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: the buffer has the size and layout the information class requires.
            if unsafe {
                SetInformationJobObject(
                    raw,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    size_of_val(&limits) as u32,
                )
            } == 0
            {
                let error = io::Error::last_os_error();
                return Err(io::Error::other(format!(
                    "cannot limit a new job object: {error}"
                )));
            }
            // Assignment fails when an enclosing job does not allow nesting.
            // SAFETY: the job handle is live, and the pseudo handle needs no closing.
            if unsafe { AssignProcessToJobObject(raw, GetCurrentProcess()) } == 0 {
                let error = io::Error::last_os_error();
                return Err(io::Error::other(format!(
                    "cannot join a new job object: {error}"
                )));
            }
            Ok(Self {
                job: ManuallyDrop::new(job),
            })
        }

        /// Terminates the job, ending this process with exit code 1 as well.
        pub(super) fn kill(&self) {
            // SAFETY: the job handle is live.
            unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) };
        }
    }
}
