use std::sync::{mpsc, Mutex};

use crate::adapter::{AdapterInstance, CoshCoreAdapter};
use crate::shell_host::ShellHostConfig;

type ProbeSender = mpsc::SyncSender<Option<bool>>;
type ProbeReceiver = mpsc::Receiver<Option<bool>>;

pub(super) fn probe_core(adapter: &AdapterInstance, ai_enabled: bool) -> Option<CoshCoreAdapter> {
    (ai_enabled && crate::runtime::startup::startup_banner_enabled())
        .then(|| match adapter {
            AdapterInstance::CoshCore(core) => Some(core.clone()),
            _ => None,
        })
        .flatten()
}

struct DeferredProbe {
    pending: Mutex<Option<(CoshCoreAdapter, ProbeSender)>>,
}

impl DeferredProbe {
    fn new(core: CoshCoreAdapter) -> (Self, ProbeReceiver) {
        let (sender, receiver) = channel();
        (
            Self {
                pending: Mutex::new(Some((core, sender))),
            },
            receiver,
        )
    }

    fn launch(&self) {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some((core, sender)) = pending {
            launch(core, sender);
        }
    }
}

/// Starts the credential probe; a managed Bash login defers it until the
/// shell reports the startup PATH that provider launches must inherit.
pub(super) fn arm(
    config: &mut ShellHostConfig,
    core: Option<CoshCoreAdapter>,
    managed_bash_login: bool,
) -> (
    Option<ProbeReceiver>,
    Option<mpsc::Receiver<Option<String>>>,
) {
    if !managed_bash_login {
        return (core.map(start), None);
    }
    let (path_sender, path_receiver) = mpsc::sync_channel(1);
    let (deferred, receiver) = core.map(DeferredProbe::new).unzip();
    config.set_shell_startup_path_observer(move |path| {
        if let Some(path) = &path {
            crate::adapter::record_trusted_startup_path(path.clone());
        }
        let _ = path_sender.try_send(path);
        if let Some(probe) = &deferred {
            probe.launch();
        }
    });
    (receiver, Some(path_receiver))
}

fn channel() -> (ProbeSender, ProbeReceiver) {
    mpsc::sync_channel(1)
}

fn start(core: CoshCoreAdapter) -> ProbeReceiver {
    let (sender, receiver) = channel();
    launch(core, sender);
    receiver
}

fn launch(core: CoshCoreAdapter, sender: ProbeSender) {
    let _ = std::thread::Builder::new()
        .name("cosh-startup-auth-probe".to_string())
        .spawn(move || {
            let _ = sender.send(core.ai_configured().ok());
        });
}
