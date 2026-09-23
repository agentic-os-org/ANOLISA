//! Telemetry module tree: default collection + self-hosted SLS upload.
//!
//! This top-level file is a thin barrel; the substantive code lives in the
//! `src/telemetry/` submodules (Rust 2018 layout, no `mod.rs`).
//!
//! Submodules:
//! - [`common`]: shared config, errors, and region detection (the common lib)
//! - [`metadata`]: ECS metadata / cloud-init client shared by region + instance probing
//! - [`channel`]: collection sentinel + idempotent ops-channel setup
//! - [`instance`]: instance metadata probing + `instance.jsonl` snapshot writing
//! - [`legacy`]: decommission the pre-self-hosted ilogtail upload channel
//! - [`ops`]: ops directory layout (dir, component .jsonl files, logrotate)
//! - [`uploader`]: self-hosted SLS uploader (lazy spawn + loop)

pub mod channel;
pub mod common;
pub mod instance;
pub mod legacy;
pub mod metadata;
pub mod ops;
pub mod uploader;

pub use channel::{DISABLE_MARKER_PATH, TelemetryChannel};
pub use common::{ProductType, RegionInfo, RegionProbe, TelemetryConfig, TelemetryError};
pub use legacy::{LegacyAccountsConfig, LegacyIlogtail};
pub use ops::OpsLayout;
pub use uploader::{Endpoint, FileOffset, Uploader, UploaderConfig, UploaderError};

#[cfg(test)]
pub(crate) use common::test_config;
