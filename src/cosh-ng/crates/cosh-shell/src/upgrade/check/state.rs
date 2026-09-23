//! Session-owned state and worker lifecycle for the startup upgrade probe.

use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use super::runner::ProbeProcesses;
use super::{
    current_executable, detect_upgrade, write_cache, DetectionResult, UpgradeNotice, UpgradeVerdict,
};

/// Startup-owned receiver state for the non-blocking upgrade probe.
#[derive(Default)]
pub(crate) struct StartupUpgradeState {
    pub(crate) pending: Option<mpsc::Receiver<Option<UpgradeNotice>>>,
    pub(crate) resolved: Option<Option<UpgradeNotice>>,
    pub(crate) rendered: bool,
    probe: Option<ProbeOwner>,
}

/// Kills the startup probe's in-flight commands once the session lets go of it.
struct ProbeOwner(Arc<ProbeProcesses>);

impl Drop for ProbeOwner {
    fn drop(&mut self) {
        self.0.terminate();
    }
}

impl StartupUpgradeState {
    /// Starts the fail-quiet background probe; the state owns its external commands.
    pub(crate) fn start_probe(&mut self, running_version: &'static str) {
        let processes = Arc::new(ProbeProcesses::default());
        let pending = spawn_startup_upgrade_probe(running_version, Arc::clone(&processes));
        self.pending = Some(pending);
        self.probe = Some(ProbeOwner(processes));
    }

    /// Kills probe commands that are still running so none outlive the shell session.
    pub(crate) fn shutdown(&mut self) {
        drop(self.probe.take());
    }

    pub(crate) fn wait_ready(&mut self, timeout: Duration) {
        if self.resolved.is_some() {
            return;
        }
        let Some(receiver) = &self.pending else {
            return;
        };
        match receiver.recv_timeout(timeout) {
            Ok(notice) => {
                self.resolved = Some(notice);
                self.pending = None;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.pending = None;
            }
        }
    }

    pub(crate) fn poll_ready(&mut self) {
        if self.resolved.is_some() {
            return;
        }
        let Some(receiver) = &self.pending else {
            return;
        };
        match receiver.try_recv() {
            Ok(notice) => {
                self.resolved = Some(notice);
                self.pending = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.pending = None;
            }
        }
    }

    pub(crate) fn notice(&self) -> Option<&UpgradeNotice> {
        self.resolved.as_ref().and_then(Option::as_ref)
    }
}

/// Starts the fail-quiet startup probe on a named worker thread.
fn spawn_startup_upgrade_probe(
    running_version: &'static str,
    processes: Arc<ProbeProcesses>,
) -> mpsc::Receiver<Option<UpgradeNotice>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let _ = thread::Builder::new()
        .name("cosh-startup-upgrade-probe".to_string())
        .spawn(move || {
            let result = current_executable()
                .and_then(|executable| detect_upgrade(&processes, running_version, &executable));
            let notice = match &result {
                Some(DetectionResult {
                    verdict: UpgradeVerdict::Upgrade(notice),
                    ..
                }) => Some(notice.clone()),
                _ => None,
            };
            let _ = sender.send(notice);
            if let Some(result) = result {
                if !matches!(result.verdict, UpgradeVerdict::Unavailable) {
                    let _ = write_cache(&result);
                }
            }
        });
    receiver
}
