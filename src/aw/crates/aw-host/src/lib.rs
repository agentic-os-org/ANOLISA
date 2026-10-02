//! Prepared structured Provider and native hook execution over bounded transport.
//! Adapters retain native scheduling, permissions and effect adoption.

#![forbid(unsafe_code)]

mod event;
mod report;
mod transport;

pub use event::Event;
pub use report::{
    CallFailure, CallRecord, Failure, FailureAction, Invocation, Method, NativeOutput,
    PreparationFailure, ProcessOutput, StepOutput,
};

use aw_config::{Configuration, Validator};
use aw_exec::{CommandSpec, Limits};
use aw_provider::{
    admission::{self, AdapterCapabilities, AdmittedStep, ProviderEvidence},
    Protocol, Reply, MAX_MESSAGE_BYTES, VERSION,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};
use transport::{check, Exchange, Transport};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Complete local execution context retained unchanged for all Provider methods.
///
/// This is supplied by the embedding application, not Provider output. The Host
/// does not inherit its own environment or provide a sandbox. Executables and
/// their dependencies must remain trusted and stable for the Host's lifetime.
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
    Admission(#[from] aw_provider::Error),
    /// Preparation or event creation exceeded its limit or was cancelled.
    #[error(transparent)]
    Execution(#[from] aw_exec::Error),
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

struct Provider {
    native: bool,
    command: CommandSpec,
    config: Value,
    timeout: Duration,
    limits: Limits,
}

/// Immutable configuration, process context and admitted steps for one Agent target.
///
/// Construct a new Host when configuration, Adapter evidence or process context
/// changes. This binding pins values, not executable files or imported code.
/// The Host starts only Provider children, never an Agent, daemon or audit writer.
pub struct Host {
    configuration: Configuration,
    revision: String,
    target: String,
    capabilities: AdapterCapabilities,
    protocol: Protocol,
    providers: BTreeMap<String, Provider>,
    steps: Vec<AdmittedStep>,
    preparation: Vec<CallRecord>,
}

impl Host {
    /// Validate desired configuration, execute discovery/private validation, then admit.
    ///
    /// All enabled requirements are preflighted before starting any command. Each
    /// structured Provider is prepared once; native commands skip discovery and
    /// private validation. Disabled-only references are skipped.
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
        let configuration = Validator::new()?.parse(bytes)?;
        let requirements = admission::preflight(&configuration, target, &capabilities)?;
        let protocol = Protocol::new()?;
        let revision = format!("{:x}", Sha256::digest(bytes));
        check(deadline, cancelled)?;
        let mut host = Self {
            configuration,
            revision,
            target: target.into(),
            capabilities,
            protocol,
            providers: BTreeMap::new(),
            steps: Vec::new(),
            preparation: Vec::new(),
        };
        for step in &requirements {
            if !host.providers.contains_key(&step.provider) {
                let value = &host.configuration.as_value()["spec"]["providers"][&step.provider];
                host.providers
                    .insert(step.provider.clone(), provider(value, &context)?);
            }
        }
        if let Err(cause) = host.prepare_providers(deadline, cancelled) {
            return Err(Error::Preparation(Box::new(PreparationFailure {
                completed: host.preparation,
                cause,
            })));
        }
        Ok(host)
    }

    fn prepare_providers(
        &mut self,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<(), Error> {
        let mut evidence = BTreeMap::new();
        for (name, provider) in &self.providers {
            if provider.native {
                continue;
            }
            let mut replies = Vec::new();
            for method in [Method::Describe, Method::ValidateConfig] {
                let record = self.record(name, method, None, None)?;
                let request_id = record.request_id.clone();
                let exchange = self.transport(provider, deadline, cancelled).exchange(record, |_| {
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
            evidence.insert(
                name.clone(),
                ProviderEvidence {
                    description,
                    configuration,
                },
            );
        }
        self.steps = admission::admit(
            &self.configuration,
            &self.target,
            &self.capabilities,
            &evidence,
        )?;
        check(deadline, cancelled)?;
        Ok(())
    }

    /// Exact-document revision retained for every exchange and invocation binding.
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// Configured target whose Adapter and event binding the Host requires.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Trusted version/entrypoint evidence retained separately from Provider claims.
    pub fn capabilities(&self) -> &AdapterCapabilities {
        &self.capabilities
    }

    /// Successful preparation exchanges, available for the embedding audit writer.
    pub fn preparation(&self) -> &[CallRecord] {
        &self.preparation
    }

    /// Admitted steps in configuration order within each event; no scheduling implied.
    pub fn steps(&self) -> &[AdmittedStep] {
        &self.steps
    }

    pub(crate) fn record(
        &self,
        provider: &str,
        method: Method,
        event_id: Option<&str>,
        step_id: Option<&str>,
    ) -> Result<CallRecord, Error> {
        Ok(CallRecord {
            config_revision: self.revision.clone(),
            binding_id: self.target.clone(),
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
        command: CommandSpec {
            program: PathBuf::from(program),
            args: args.iter().map(OsString::from).collect(),
            cwd: context.cwd.clone(),
            environment: context.environment.clone(),
        },
        config: value["config"].clone(),
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
