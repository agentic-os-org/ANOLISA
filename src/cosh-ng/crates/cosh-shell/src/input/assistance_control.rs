use std::fs::{self, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub(crate) struct AssistanceControl {
    enabled: Arc<AtomicBool>,
    at_prompt: Arc<AtomicBool>,
    state_file: PathBuf,
}

impl AssistanceControl {
    pub(crate) fn enabled(state_file: PathBuf) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(true)),
            at_prompt: Arc::new(AtomicBool::new(false)),
            state_file,
        }
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub(crate) fn set_at_prompt(&self, at_prompt: bool) {
        self.at_prompt.store(at_prompt, Ordering::Release);
    }

    pub(crate) fn is_at_prompt(&self) -> bool {
        self.at_prompt.load(Ordering::Acquire)
    }

    pub(crate) fn toggle(&self) -> std::io::Result<bool> {
        self.set_enabled(!self.is_enabled())
    }

    pub(crate) fn set_enabled(&self, enabled: bool) -> std::io::Result<bool> {
        if enabled {
            if let Err(error) = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&self.state_file)
            {
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
        } else if let Err(error) = fs::remove_file(&self.state_file) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(error);
            }
        }
        self.enabled.store(enabled, Ordering::Release);
        Ok(enabled)
    }
}

impl PartialEq for AssistanceControl {
    fn eq(&self, other: &Self) -> bool {
        self.state_file == other.state_file
            && self.is_enabled() == other.is_enabled()
            && self.is_at_prompt() == other.is_at_prompt()
    }
}

impl Eq for AssistanceControl {}
