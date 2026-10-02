// SPDX-License-Identifier: Apache-2.0
//! Unified error type for `blaze-core`.

use std::path::PathBuf;

use thiserror::Error;

/// Convenient `Result` alias defaulting to [`BlazeError`].
pub type Result<T> = std::result::Result<T, BlazeError>;

#[derive(Debug, Error)]
pub enum BlazeError {
    #[error("failed to load policy from {path}: {source}")]
    PolicyLoadError {
        path: PathBuf,
        #[source]
        source: Box<BlazeError>,
    },

    #[error("policy evaluation failed: {reason}")]
    PolicyEvalError { reason: String },

    #[error("no available backend for request: requested={requested:?}, available={available:?}")]
    BackendUnavailable {
        requested: Vec<String>,
        available: Vec<String>,
    },

    #[error("invalid sandbox state transition: {from} -> {to}")]
    InvalidStateTransition { from: String, to: String },

    /// A lifecycle caller tried to replace an unfinished durable operation.
    #[error("sandbox operation already in progress: active={active}, requested={requested}")]
    OperationInProgress { active: String, requested: String },

    #[error("hook '{hook_name}' error: {msg}")]
    HookError { hook_name: String, msg: String },

    #[error("config error: {source}")]
    ConfigError {
        #[source]
        source: ConfigErrorSource,
    },

    #[error("io error: {source}")]
    IoError {
        #[source]
        source: std::io::Error,
    },

    #[error("backend error: {msg}")]
    BackendError { msg: String },

    #[error("storage error: {msg}")]
    StorageError { msg: String },

    /// A previously allocated storage slot has lost a required artifact.
    #[error("storage slot '{instance_id}' is incomplete: expected {expected} at {path}")]
    StorageIncomplete {
        instance_id: String,
        path: PathBuf,
        expected: &'static str,
    },
}

/// Internal wrapper that lets [`BlazeError::ConfigError`] carry either a
/// TOML deserialization error or a JSON one without leaking those types
/// to public APIs.
#[derive(Debug, Error)]
pub enum ConfigErrorSource {
    #[error("toml parse error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("json parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("invalid value: {0}")]
    InvalidValue(String),
}

impl From<std::io::Error> for BlazeError {
    fn from(source: std::io::Error) -> Self {
        BlazeError::IoError { source }
    }
}

impl From<toml::de::Error> for BlazeError {
    fn from(err: toml::de::Error) -> Self {
        BlazeError::ConfigError {
            source: ConfigErrorSource::Toml(err),
        }
    }
}

impl From<serde_json::Error> for BlazeError {
    fn from(err: serde_json::Error) -> Self {
        BlazeError::ConfigError {
            source: ConfigErrorSource::Json(err),
        }
    }
}
