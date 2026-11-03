//! Native loading and AW capability admission retain separate evidence and ownership.

use aw_config::{
    native::{source_report, NativeSource, SourceReport},
    Configuration,
};
use aw_provider::admission::{self, AdapterCapabilities, AdmittedStep, ProviderEvidence};
use serde::Serialize;
use std::collections::BTreeMap;

/// Validated references and policy for an embedding Adapter's native loader.
///
/// Keep native cwd, environment, argv, permissions and loader options in the
/// caller's context. AW neither parses the native files nor synthesizes replacement
/// settings. An empty source list retains the loader's default discovery behavior.
pub struct NativeConfiguration<'a> {
    config: &'a Configuration,
    target: &'a str,
    sources: Vec<NativeSource>,
}

impl<'a> NativeConfiguration<'a> {
    /// Select one validated target without touching any native source.
    ///
    /// # Errors
    /// Rejects an absent target.
    pub fn new(config: &'a Configuration, target: &'a str) -> Result<Self, aw_config::Error> {
        Ok(Self {
            sources: config.native_sources(target)?,
            config,
            target,
        })
    }

    /// Invoke the native loader exactly once with unchanged ordered references.
    ///
    /// The loader retains unknown fields, native object names, Hook order,
    /// precedence and credential references. Its returned value may be a live
    /// configuration handle, preserving native watcher/update semantics. The
    /// caller must report the sources actually consumed; descriptors alone are
    /// not evidence that an existing launcher supports new source options.
    ///
    /// # Errors
    /// Propagates the native loader's own error unchanged. No AW admission is
    /// attempted on failure, and no native error becomes a permissive default.
    pub fn load<T, E>(
        self,
        loader: impl FnOnce(&[NativeSource]) -> Result<NativeLoad<T>, E>,
    ) -> Result<LoadedConfiguration<'a, T>, E> {
        let loaded = loader(&self.sources)?;
        Ok(LoadedConfiguration {
            input: self,
            native: loaded.value,
            sources: loaded.sources,
        })
    }
}

/// Native loader result and the actual sources it consumed, in native order.
///
/// The trusted Adapter supplies this evidence after loading, including implicit
/// sources it discovered. Do not echo desired descriptors for absent or ignored
/// sources. References stay private and are redacted in public reports.
pub struct NativeLoad<T> {
    /// Original native value or live update handle.
    pub value: T,
    /// Sources actually consumed by the native loader, in its precedence order.
    pub sources: Vec<NativeSource>,
}

/// Native loader output retained independently of AW policy admission.
pub struct LoadedConfiguration<'a, T> {
    input: NativeConfiguration<'a>,
    native: T,
    sources: Vec<NativeSource>,
}

impl<'a, T> LoadedConfiguration<'a, T> {
    /// Borrow the original native configuration or live loader handle.
    pub fn native(&self) -> &T {
        &self.native
    }

    /// Return the original loader output, including any native update ownership.
    pub fn into_native(self) -> T {
        self.native
    }

    /// Report successful native loading; no AW capability or activation is claimed.
    pub fn report(&self) -> ConfigurationReport {
        ConfigurationReport {
            native: "loaded",
            aw: "not_admitted",
            sources: source_report(&self.sources),
            admitted_steps: BTreeMap::new(),
        }
    }

    /// Fix AW coverage from trusted Adapter evidence and Provider admission.
    ///
    /// Reuses the existing admission contract, including exact selectors and
    /// Provider validation. Never enumerate native tools or run Hooks to infer
    /// coverage. Failure leaves this loaded native configuration available and
    /// must block activation of the requested AW policy. Existing runtime owners
    /// retain their old policy until the normal atomic update contract succeeds.
    ///
    /// # Errors
    /// Propagates admission failures without reclassifying requested operations
    /// as uncovered. Success proves admission only; callers still require native
    /// route adoption and the service's runtime activation gate before execution.
    pub fn admit(
        &self,
        capabilities: &AdapterCapabilities,
        providers: &BTreeMap<String, ProviderEvidence>,
    ) -> Result<ConfigurationCoverage<'a>, aw_provider::Error> {
        let steps = admission::admit(
            self.input.config,
            self.input.target,
            capabilities,
            providers,
        )?;
        Ok(ConfigurationCoverage {
            config: self.input.config,
            target: self.input.target,
            steps,
            sources: source_report(&self.sources),
        })
    }
}

/// Immutable admitted scope; invocation failures cannot mutate it into noncoverage.
///
/// Construction requires the common admission path. This object is not serialized
/// for callback input and is not proof that native routes are active. Runtime
/// consumers retain it with their policy revision and execute its admitted steps.
pub struct ConfigurationCoverage<'a> {
    config: &'a Configuration,
    target: &'a str,
    steps: Vec<AdmittedStep>,
    sources: Vec<SourceReport>,
}

impl ConfigurationCoverage<'_> {
    /// Exact validated AW policy from which this coverage was admitted.
    pub fn configuration(&self) -> &Configuration {
        self.config
    }

    /// Configured target whose trusted Adapter evidence was checked.
    pub fn target(&self) -> &str {
        self.target
    }

    /// Admitted event/step selector intersections, in configured step order.
    pub fn steps(&self) -> &[AdmittedStep] {
        &self.steps
    }

    /// Report admitted scope without native values, selectors or Provider settings.
    pub fn report(&self) -> ConfigurationReport {
        let mut admitted_steps = BTreeMap::new();
        for step in &self.steps {
            *admitted_steps.entry(step.event.clone()).or_insert(0) += 1;
        }
        ConfigurationReport {
            native: "loaded",
            aw: "admitted_not_active",
            sources: self.sources.clone(),
            admitted_steps,
        }
    }
}

/// Sanitized evidence boundaries; native scope outside admitted steps stays native.
#[derive(Debug, Serialize)]
pub struct ConfigurationReport {
    /// Native load state, independent of AW policy admission.
    pub native: &'static str,
    /// AW admission state; activation requires service-owned runtime evidence.
    pub aw: &'static str,
    /// Consumed source categories and ordinal positions, without references or values.
    pub sources: Vec<SourceReport>,
    /// Admitted step count by event; exact scope remains in the typed steps.
    pub admitted_steps: BTreeMap<String, usize>,
}
