//! Internal native Agent integration, compiled and released with AW.
//! The caller owns service bindings, process lifetime, signals and launch files.

#![deny(missing_docs)]

mod adapter;
mod adapters;
pub mod association;
pub mod attachment;
pub mod callback;
pub mod completion;
pub mod projection;
mod qoder;
pub mod reload;

pub use adapter::{
    block_reason, select, Adapter, HookEvent, HookOutput, InstallOptions, LaunchContext,
    LaunchFiles, LaunchInput, LaunchPlan, NativeContext, NativeOptions, PreparedLaunch, Readiness,
    OBSERVE_STEP,
};

/// Native callback status; only the CLI delivers signals or exits its process.
pub enum NativeExit {
    /// Native numeric exit code.
    Code(i32),
    /// Native terminating signal.
    Signal(i32),
}

use NativeExit as Exit;

/// Native integration failure; service and wire errors stay with the caller.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A native filesystem path contains a NUL byte.
    #[error(transparent)]
    Nul(#[from] std::ffi::NulError),
    /// Native input or configuration violates the selected integration contract.
    #[error("{0}")]
    Invalid(String),
    /// Reading or writing native files failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Native JSON could not be decoded or encoded.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Native YAML could not be decoded or encoded.
    #[error(transparent)]
    Yaml(#[from] serde_yaml_ng::Error),
    /// A bounded native probe failed.
    #[error(transparent)]
    Exec(#[from] aw_exec::Error),
    /// Native output is not UTF-8.
    #[error(transparent)]
    Utf8(#[from] std::str::Utf8Error),
    /// A native path or file is not UTF-8.
    #[error(transparent)]
    StringUtf8(#[from] std::string::FromUtf8Error),
    /// Native import paths cannot be joined.
    #[error(transparent)]
    JoinPaths(#[from] std::env::JoinPathsError),
}

impl From<String> for Error {
    fn from(message: String) -> Self {
        Self::Invalid(message)
    }
}

impl From<&str> for Error {
    fn from(message: &str) -> Self {
        Self::Invalid(message.into())
    }
}

/// Result of native preparation, normalization or effect adoption.
pub type Result<T> = std::result::Result<T, Error>;

fn read_file(path: impl AsRef<std::path::Path>, limit: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err("file exceeds byte limit".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod test_support;
