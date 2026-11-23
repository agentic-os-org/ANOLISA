//! Offline SDK deployment compilation using existing Hook capability admission.

use crate::admission::{AdapterCapabilities, AdmissionEngine, AdmissionError};
use aw_contracts::deployment::VERSION;
pub use aw_contracts::deployment::{Availability, DeploymentDescriptor};
use serde_json::json;
use std::path::Path;

/// Deployment compilation failures without configuration values in diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum DescriptorError {
    /// Configuration or supplied profile failed static Hook admission.
    #[error(transparent)]
    Admission(#[from] AdmissionError),
    /// The compiled document violates the deployment contract.
    #[error(transparent)]
    Contract(#[from] aw_contracts::Error),
    /// Deployment requires resolvable locations accepted by the local service.
    #[error("deployment descriptor requires valid explicit service locations")]
    Endpoint,
    /// Native fallback conflicts with an applicable enabled required event.
    #[error("native fallback conflicts with an enabled required event")]
    Availability,
    /// A supplied hint differs from compilation of the exact deployment inputs.
    #[error("deployment descriptor does not match the configuration, target or profile")]
    Mismatch,
}

/// Optional startup hints requested by the caller's Adapter deployment profile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompileOptions {
    /// Emit `observe_session_start: true` when `session.start` is applicable.
    pub session_start_hint: bool,
}

/// Compiles deployment hints without probing Providers, installing or activating Hooks.
///
/// The caller supplies an existing trusted Adapter profile and its availability
/// choice. Static admission checks all applicable enabled events and steps.
/// Required-event availability checks use the same target projection as admission,
/// retaining empty events and uncertain native aliases.
///
/// # Errors
/// Rejects invalid configuration, unsupported profile requirements, unresolved
/// endpoints and availability conflicts. A successful compile does not establish
/// Provider admission, installed wiring, Service Ready or native adoption.
pub fn compile(
    bytes: &[u8],
    target: &str,
    capabilities: AdapterCapabilities,
    availability: Availability,
) -> Result<DeploymentDescriptor, DescriptorError> {
    compile_with_options(
        bytes,
        target,
        capabilities,
        availability,
        CompileOptions::default(),
    )
}

/// Compiles deployment hints with explicit profile-declared [`CompileOptions`].
///
/// # Errors
/// Rejects invalid configuration, unsupported profile requirements, unresolved
/// endpoints and availability conflicts.
pub fn compile_with_options(
    bytes: &[u8],
    target: &str,
    capabilities: AdapterCapabilities,
    availability: Availability,
    options: CompileOptions,
) -> Result<DeploymentDescriptor, DescriptorError> {
    let candidate = AdmissionEngine::new()?.preflight(bytes, target, capabilities)?;
    let config = candidate.configuration();
    let spec = &config.as_value()["spec"];
    let requirements =
        aw_config::requirements::project(config, target).map_err(AdmissionError::Configuration)?;
    if availability == Availability::NativeFallback
        && requirements.iter().any(|event| event.required)
    {
        return Err(DescriptorError::Availability);
    }
    let daemon = &spec["daemon"];
    let state = daemon["state_dir"]
        .as_str()
        .ok_or(DescriptorError::Endpoint)?;
    let endpoint = daemon["endpoint"]
        .as_str()
        .ok_or(DescriptorError::Endpoint)?;
    let (_, endpoint) = aw_config::service_paths::resolve(Path::new(state), Path::new(endpoint))
        .map_err(|_| DescriptorError::Endpoint)?;
    let profile = candidate.capabilities();
    let mut value = json!({
        "version": VERSION,
        "configuration_sha256": candidate.revision(),
        "target": candidate.target(),
        "profile": {
            "adapter": profile.adapter,
            "version": profile.version,
            "entrypoint": profile.entrypoint,
        },
        "endpoint": endpoint,
        "wait_for_input_submit": requirements.iter().any(|event| event.event == "input.submit"),
        "availability": availability,
    });
    if options.session_start_hint
        && requirements
            .iter()
            .any(|event| event.event == "session.start")
    {
        value["observe_session_start"] = json!(true);
    }
    let encoded = aw_contracts::canonical::bytes(&value)?;
    Ok(DeploymentDescriptor::parse(&encoded)?)
}

/// Recompiles and compares every deployment hint against authoritative inputs.
///
/// Use at deployment verification time. SDK startup can check the exact-byte
/// digest and trusted target/profile identity without parsing YAML or invoking
/// a compiler; the server must still perform runtime admission.
///
/// # Errors
/// Returns compilation errors for invalid inputs or [`DescriptorError::Mismatch`]
/// when a well-shaped descriptor contains stale or changed hints.
pub fn verify(
    descriptor: &DeploymentDescriptor,
    bytes: &[u8],
    target: &str,
    capabilities: AdapterCapabilities,
    availability: Availability,
) -> Result<(), DescriptorError> {
    verify_with_options(
        descriptor,
        bytes,
        target,
        capabilities,
        availability,
        CompileOptions::default(),
    )
}

/// Recompiles with explicit [`CompileOptions`] and compares every deployment hint.
///
/// # Errors
/// Returns compilation errors for invalid inputs or [`DescriptorError::Mismatch`]
/// when a well-shaped descriptor contains stale or changed hints.
pub fn verify_with_options(
    descriptor: &DeploymentDescriptor,
    bytes: &[u8],
    target: &str,
    capabilities: AdapterCapabilities,
    availability: Availability,
    options: CompileOptions,
) -> Result<(), DescriptorError> {
    if descriptor != &compile_with_options(bytes, target, capabilities, availability, options)? {
        return Err(DescriptorError::Mismatch);
    }
    Ok(())
}
