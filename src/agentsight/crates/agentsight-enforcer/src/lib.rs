//! Privileged enforcement service for AgentSight.

#[cfg(feature = "actplane")]
mod actplane;
mod backend;
mod event_hub;
#[cfg(feature = "mock-backend")]
mod mock;
mod service;

#[cfg(feature = "actplane")]
pub use actplane::{ActPlaneBackend, compile_credential_exfiltration_policy};
pub use backend::{BackendError, EnforcementBackend};
pub use event_hub::{EventHub, SubscriberClass};
#[cfg(feature = "mock-backend")]
pub use mock::MockBackend;
pub use service::{EnforcerService, ServiceError};
