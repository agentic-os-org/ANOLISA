//! Prepared structured Provider and native hook execution over bounded transport.
//! Adapters retain native scheduling, permissions and effect adoption.

#![forbid(unsafe_code)]

mod event;
mod report;
mod transport;

pub use aw_core::admission::AdapterCapabilities;
pub use event::Event;
pub use report::{
    CallFailure, CallRecord, Failure, FailureAction, Invocation, Method, NativeOutput,
    PreparationFailure, ProcessOutput, StepOutput,
};

use aw_config::Configuration;
use aw_core::admission::{
    AdmissionEngine, AdmissionError, AdmittedPolicy, AdmittedStep, PolicyCandidate,
    ProviderEvidence,
};
use aw_exec::{CommandSpec, Limits, PackageLeases};
use aw_provider::{Protocol, Reply, MAX_MESSAGE_BYTES, VERSION};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};
use transport::{check, Exchange, Transport};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Maximum combined key/value bytes in an explicit native callback environment.
///
/// Includes one `=` and trailing NUL per entry. The snapshot also permits at
/// most 4096 entries and rejects empty keys, `=` in keys, and embedded NULs.
pub const MAX_NATIVE_ENVIRONMENT_BYTES: usize = MAX_MESSAGE_BYTES;

/// Complete local execution context retained unchanged for all Provider methods.
///
/// This is supplied by the embedding application, not Provider output. The Host
/// does not inherit its own environment or provide a sandbox. Executables and
/// their dependencies must remain trusted and stable for the Host's lifetime.
#[derive(Clone)]
pub struct ProcessContext {
    /// Absolute Agent-side working directory for all configured Provider commands.
    pub cwd: PathBuf,
    /// Complete child environment, including an explicit PATH if it is needed.
    pub environment: BTreeMap<OsString, OsString>,
    /// Independent diagnostic-stream cap; diagnostics are never auto-logged.
    pub stderr_bytes: usize,
}

/// Preparation or event-selection error. Invocation failures remain in [`Invocation`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid desired configuration; no Provider process was started.
    #[error(transparent)]
    Configuration(#[from] aw_config::Error),
    /// Invalid admission evidence or configuration capability requirements.
    #[error(transparent)]
    Admission(AdmissionError),
    /// Bundled Provider protocol schema failed to compile.
    #[error(transparent)]
    Protocol(#[from] aw_provider::Error),
    /// Preparation or event creation exceeded its limit or was cancelled.
    #[error(transparent)]
    Execution(#[from] aw_exec::Error),
    /// A referenced installed package cannot be protected from concurrent mutation.
    #[error(transparent)]
    PackageLease(#[from] aw_exec::LeaseError),
    /// A preparation exchange failed; includes exact call correlation and status.
    #[error(transparent)]
    Call(Box<CallFailure>),
    /// Preparation history and its cause, including failures in final admission.
    #[error(transparent)]
    Preparation(Box<PreparationFailure>),
    /// Invalid local context, event identity, step selection or repeated invocation.
    #[error("invalid Provider Host context: {0}")]
    Invalid(&'static str),
}

impl From<AdmissionError> for Error {
    fn from(error: AdmissionError) -> Self {
        match error {
            AdmissionError::Configuration(error) => Self::Configuration(error),
            error @ AdmissionError::Rejected(_) => Self::Admission(error),
        }
    }
}

struct Provider {
    native: bool,
    hook: bool,
    command: CommandSpec,
    config: Value,
    timeout: Duration,
    limits: Limits,
}

/// Discovered Provider resources, protocol evidence, and managed file leases for one candidate.
///
/// External callers obtain [`PreparedProviders`] exclusively from [`Host::discover`]
/// and cannot construct or deserialize it directly:
///
/// ```compile_fail
/// let prepared: aw_host::PreparedProviders =
///     serde_json::from_str("{}").unwrap();
/// ```
pub struct PreparedProviders {
    revision: String,
    target: String,
    capabilities: AdapterCapabilities,
    protocol: Protocol,
    providers: BTreeMap<String, Provider>,
    evidence: BTreeMap<String, ProviderEvidence>,
    preparation: Vec<CallRecord>,
    _package_leases: PackageLeases,
}

impl PreparedProviders {
    /// Protocol-checked Provider evidence collected during discovery.
    pub fn evidence(&self) -> &BTreeMap<String, ProviderEvidence> {
        &self.evidence
    }

    /// Successful preparation exchanges recorded during discovery.
    pub fn preparation(&self) -> &[CallRecord] {
        &self.preparation
    }

    /// Exact-document revision bound to these discovered Provider resources.
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// Target Agent key bound to these discovered Provider resources.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Trusted Adapter capabilities bound to these discovered Provider resources.
    pub fn capabilities(&self) -> &AdapterCapabilities {
        &self.capabilities
    }

    /// Consume discovered Provider state into a preparation failure retaining completed calls.
    pub fn into_failure(self, cause: Error) -> Error {
        Error::Preparation(Box::new(PreparationFailure {
            completed: self.preparation,
            cause,
        }))
    }
}

/// Immutable configuration, process context and admitted steps for one Agent target.
///
/// Construct a new Host when configuration, Adapter evidence or process context
/// changes. Managed package locks live until the last owner releases this Host;
/// unmanaged executables and imported code remain the caller's responsibility.
/// The Host starts only Provider children, never an Agent, daemon or audit writer.
pub struct Host {
    policy: AdmittedPolicy,
    protocol: Protocol,
    providers: BTreeMap<String, Provider>,
    preparation: Vec<CallRecord>,
    _package_leases: PackageLeases,
}

impl Host {
    /// Discover and validate structured Providers required by a preflighted candidate.
    ///
    /// Uses the candidate's already-validated configuration and step requirements
    /// without re-parsing configuration bytes. Structured Providers run `describe`
    /// and `validate_config` once; boolean hooks and native commands skip discovery.
    ///
    /// # Errors
    /// Rejects invalid local context, package lease conflicts, and any failed,
    /// cancelled, or expired exchange. Completed exchanges before a discovery failure
    /// are retained in [`Error::Preparation`] while managed package leases are released.
    pub fn discover(
        candidate: &PolicyCandidate,
        context: ProcessContext,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<PreparedProviders, Error> {
        check(deadline, cancelled)?;
        if !context.cwd.is_absolute() {
            return Err(Error::Invalid("working directory must be absolute"));
        }
        let protocol = Protocol::new()?;
        let mut providers = BTreeMap::new();
        for step in candidate.requirements() {
            if !providers.contains_key(&step.provider) {
                let value =
                    &candidate.configuration().as_value()["spec"]["providers"][&step.provider];
                providers.insert(step.provider.clone(), provider(value, &context)?);
            }
        }
        let package_leases =
            PackageLeases::acquire(providers.values_mut().map(|provider| &mut provider.command))?;
        check(deadline, cancelled)?;
        let mut prepared = PreparedProviders {
            revision: candidate.revision().to_owned(),
            target: candidate.target().to_owned(),
            capabilities: candidate.capabilities().clone(),
            protocol,
            providers,
            evidence: BTreeMap::new(),
            preparation: Vec::new(),
            _package_leases: package_leases,
        };
        if let Err(cause) = prepared.discover_providers(deadline, cancelled) {
            return Err(Error::Preparation(Box::new(PreparationFailure {
                completed: prepared.preparation,
                cause,
            })));
        }
        Ok(prepared)
    }

    /// Construct an immutable Host from an admitted policy and its discovered Providers.
    ///
    /// Verifies that the admitted policy and discovered Provider resources share
    /// the same configuration revision, target, Adapter capabilities, Provider
    /// evidence, and required Provider commands.
    ///
    /// # Errors
    /// Rejects mismatched revision, target, Adapter capabilities, Provider
    /// evidence, or missing prepared Provider resources.
    pub fn from_admitted(
        policy: AdmittedPolicy,
        prepared: PreparedProviders,
    ) -> Result<Self, Error> {
        if policy.revision() != prepared.revision
            || policy.target() != prepared.target
            || !same_capabilities(policy.capabilities(), &prepared.capabilities)
            || !same_evidence(policy.providers(), &prepared.evidence)
            || policy
                .steps()
                .iter()
                .any(|step| !prepared.providers.contains_key(&step.provider))
        {
            return Err(Error::Invalid(
                "admitted policy does not match prepared Providers",
            ));
        }
        Ok(Self {
            policy,
            protocol: prepared.protocol,
            providers: prepared.providers,
            preparation: prepared.preparation,
            _package_leases: prepared._package_leases,
        })
    }

    /// Validate desired configuration, execute discovery/private validation, then admit.
    ///
    /// All enabled requirements are preflighted before starting any command. Each
    /// structured Provider is prepared once. Boolean hooks use local effect
    /// admission and default to invoke without discovery/private-validation calls;
    /// native commands also skip those calls. Disabled-only references are skipped.
    /// Preparation shares the caller's absolute deadline, with each exchange also
    /// capped by the configured Provider timeout. Parsing, encoding and response
    /// validation count against it. Transport cleanup retains its separate budget.
    /// Successful discovery proves protocol agreement, not native hook installation.
    ///
    /// # Errors
    /// Rejects invalid context, configuration or admission and any failed, cancelled
    /// or late exchange. No partially prepared Host is returned. Earlier completed
    /// calls are retained in [`Error::Preparation`] and are not rolled back;
    /// Providers should keep preparation side-effect free.
    pub fn prepare(
        bytes: &[u8],
        target: &str,
        capabilities: AdapterCapabilities,
        context: ProcessContext,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Self, Error> {
        check(deadline, cancelled)?;
        if !context.cwd.is_absolute() {
            return Err(Error::Invalid("working directory must be absolute"));
        }
        let engine = AdmissionEngine::new()?;
        let candidate = engine.preflight(bytes, target, capabilities)?;
        let prepared = Self::discover(&candidate, context, deadline, cancelled)?;
        let policy = match engine.admit(candidate, prepared.evidence()) {
            Ok(policy) => policy,
            Err(error) => return Err(prepared.into_failure(error.into())),
        };
        if let Err(error) = check(deadline, cancelled) {
            return Err(prepared.into_failure(error.into()));
        }
        Self::from_admitted(policy, prepared)
    }

    /// Admitted policy snapshot retained by this Host.
    pub fn policy(&self) -> &AdmittedPolicy {
        &self.policy
    }

    /// Validated immutable configuration retained by this Host snapshot.
    pub fn configuration(&self) -> &Configuration {
        self.policy.configuration()
    }

    /// Exact-document revision retained for every exchange and invocation binding.
    pub fn revision(&self) -> &str {
        self.policy.revision()
    }

    /// Configured target whose Adapter and event binding the Host requires.
    pub fn target(&self) -> &str {
        self.policy.target()
    }

    /// Trusted version/entrypoint evidence retained separately from Provider claims.
    pub fn capabilities(&self) -> &AdapterCapabilities {
        self.policy.capabilities()
    }

    /// Successful preparation exchanges, available for the embedding audit writer.
    pub fn preparation(&self) -> &[CallRecord] {
        &self.preparation
    }

    /// Admitted steps in configuration order within each event; no scheduling implied.
    pub fn steps(&self) -> &[AdmittedStep] {
        self.policy.steps()
    }

    pub(crate) fn record(
        &self,
        provider: &str,
        method: Method,
        event_id: Option<&str>,
        step_id: Option<&str>,
    ) -> Result<CallRecord, Error> {
        Ok(CallRecord {
            config_revision: self.revision().to_owned(),
            binding_id: self.target().to_owned(),
            provider: provider.into(),
            request_id: identifier()?,
            method,
            event_id: event_id.map(str::to_owned),
            step_id: step_id.map(str::to_owned),
            elapsed: Duration::ZERO,
            process: None,
        })
    }

    fn transport<'a>(
        &'a self,
        provider: &'a Provider,
        deadline: Instant,
        cancelled: &'a AtomicBool,
    ) -> Transport<'a> {
        Transport {
            protocol: &self.protocol,
            command: &provider.command,
            limits: provider.limits,
            deadline: deadline.min(Instant::now() + provider.timeout),
            cancelled,
        }
    }
}

impl PreparedProviders {
    fn discover_providers(
        &mut self,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<(), Error> {
        for (name, provider) in &self.providers {
            if provider.native || provider.hook {
                continue;
            }
            let mut replies = Vec::new();
            for method in [Method::Describe, Method::ValidateConfig] {
                let record = CallRecord {
                    config_revision: self.revision.clone(),
                    binding_id: self.target.clone(),
                    provider: name.clone(),
                    request_id: identifier()?,
                    method,
                    event_id: None,
                    step_id: None,
                    elapsed: Duration::ZERO,
                    process: None,
                };
                let request_id = record.request_id.clone();
                let transport = Transport {
                    protocol: &self.protocol,
                    command: &provider.command,
                    limits: provider.limits,
                    deadline: deadline.min(Instant::now() + provider.timeout),
                    cancelled,
                };
                let exchange = transport.exchange(record, |_| {
                    let mut value = json!({"api_version": VERSION, "request_id": request_id, "method": method.as_str()});
                    if method == Method::ValidateConfig {
                        value["config"] = provider.config.clone();
                    }
                    self.protocol.parse_request(&encode(&value)?)
                });
                match exchange.result {
                    Ok(reply) => {
                        self.preparation.push(exchange.record);
                        replies.push(reply);
                    }
                    Err(failure) => {
                        return Err(Error::Call(Box::new(CallFailure {
                            record: exchange.record,
                            failure,
                        })))
                    }
                }
            }
            let mut replies = replies.into_iter();
            let (Some(Reply::Description(description)), Some(Reply::Configuration(configuration))) =
                (replies.next(), replies.next())
            else {
                return Err(Error::Invalid("unexpected preparation reply type"));
            };
            self.evidence.insert(
                name.clone(),
                ProviderEvidence {
                    description,
                    configuration,
                },
            );
        }
        check(deadline, cancelled)?;
        Ok(())
    }
}

fn same_capabilities(left: &AdapterCapabilities, right: &AdapterCapabilities) -> bool {
    left.adapter == right.adapter
        && left.version == right.version
        && left.entrypoint == right.entrypoint
        && left.events == right.events
        && left.failure_actions == right.failure_actions
}

fn same_evidence(
    left: &BTreeMap<String, ProviderEvidence>,
    right: &BTreeMap<String, ProviderEvidence>,
) -> bool {
    left.len() == right.len()
        && left.iter().all(|(name, left)| {
            right.get(name).is_some_and(|right| {
                left.description.as_value() == right.description.as_value()
                    && left.configuration.as_value() == right.configuration.as_value()
            })
        })
}

fn provider(value: &Value, context: &ProcessContext) -> Result<Provider, Error> {
    let argv = value["transport"]["argv"]
        .as_array()
        .ok_or(Error::Invalid("missing Provider argv"))?;
    let strings: Vec<_> = argv
        .iter()
        .map(|part| {
            part.as_str()
                .ok_or(Error::Invalid("invalid Provider argument"))
        })
        .collect::<Result<_, _>>()?;
    let (program, args) = strings
        .split_first()
        .ok_or(Error::Invalid("empty Provider argv"))?;
    let output_bytes = positive(&value["max_output_bytes"])?;
    Ok(Provider {
        native: value["protocol"] == "native-hook/v1alpha1",
        hook: value["protocol"]
            .as_str()
            .unwrap_or(aw_config::HOOK_PROTOCOL)
            == aw_config::HOOK_PROTOCOL,
        command: CommandSpec {
            program: PathBuf::from(program),
            args: args.iter().map(OsString::from).collect(),
            cwd: context.cwd.clone(),
            environment: context.environment.clone(),
        },
        config: value.get("config").cloned().unwrap_or_else(|| json!({})),
        timeout: Duration::from_millis(positive(&value["timeout_ms"])?),
        limits: Limits {
            input_bytes: MAX_MESSAGE_BYTES,
            stdout_bytes: output_bytes.min(MAX_MESSAGE_BYTES as u64) as usize,
            stderr_bytes: context.stderr_bytes,
        },
    })
}

fn positive(value: &Value) -> Result<u64, Error> {
    // JSON Schema integers include integral decimal/exponent forms such as 1000.0.
    // Configuration limits are at most u32::MAX, exactly representable as f64.
    value
        .as_f64()
        .filter(|value| *value >= 1.0 && *value <= f64::from(u32::MAX) && value.fract() == 0.0)
        .map(|value| value as u64)
        .ok_or(Error::Invalid("missing positive budget or limit"))
}

fn identifier() -> Result<String, Error> {
    let value = NEXT_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map_err(|_| Error::Invalid("request identifier space exhausted"))?;
    Ok(format!("aw-{}-{value}", std::process::id()))
}

fn encode(value: &Value) -> Result<Vec<u8>, aw_provider::Error> {
    serde_json::to_vec(value).map_err(|_| aw_provider::Error::Invalid("request encoding failed"))
}
