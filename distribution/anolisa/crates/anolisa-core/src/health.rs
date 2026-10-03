//! Health-check engine: structured probes shared across enable/status/doctor.
//!
//! [`CheckSpec`] is the declarative `[component.health_check]` form from the
//! minimal component schema; [`run_check`] executes it under path/timeout
//! guards and returns a [`CheckOutcome`] tree.
//! v1 implements binary/file/command probes plus `all_of`/`any_of`; the
//! remaining variants report [`CheckStatus::Unsupported`] until the owning
//! slice (systemd, ports, HTTP, capabilities) lands.

mod engine;
mod spec;

pub use engine::{CheckEnv, ServiceProbes, run_check};
pub use spec::{CheckOutcome, CheckSpec, CheckStatus, Protocol};
