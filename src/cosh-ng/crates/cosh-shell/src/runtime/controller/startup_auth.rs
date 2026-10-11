use std::sync::{mpsc, Mutex};

use crate::adapter::CoshCoreAdapter;

pub(super) type ProbeSender = mpsc::SyncSender<Option<bool>>;
pub(super) type ProbeReceiver = mpsc::Receiver<Option<bool>>;

pub(super) struct DeferredProbe {
    pending: Mutex<Option<(CoshCoreAdapter, ProbeSender)>>,
}

impl DeferredProbe {
    pub(super) fn new(core: CoshCoreAdapter) -> (Self, ProbeReceiver) {
        let (sender, receiver) = channel();
        (
            Self {
                pending: Mutex::new(Some((core, sender))),
            },
            receiver,
        )
    }

    pub(super) fn launch(&self) {
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

fn channel() -> (ProbeSender, ProbeReceiver) {
    mpsc::sync_channel(1)
}

pub(super) fn start(core: CoshCoreAdapter) -> ProbeReceiver {
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
