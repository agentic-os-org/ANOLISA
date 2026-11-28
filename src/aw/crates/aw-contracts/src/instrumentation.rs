//! Versioned Python/JS instrumentation profile contracts and Source capability compiler.
//!
//! An [`InstrumentationProfile`] declaratively describes which functions or
//! methods in a Python or JS SDK to wrap, which semantic Hooks they serve, and
//! where to read or write Hook fields. It compiles purely to a
//! [`SourceCapability`] (`python` or `js`) so the offline resolver can reason
//! about it without probing processes or loading runtime wrappers. Validation
//! establishes shape and semantic invariants only, never installed wiring or
//! runtime adoption.

pub mod builtin;
mod validate;

use crate::{
    canonical, require,
    source::{
        shared_registry, FailureAction, HookAuthority, HookEffect, HookPointCapability,
        SourceCapability, SourceKind,
    },
    Error, Registry,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Version of the instrumentation profile contract understood by this build.
pub const VERSION: &str = "aw-instrumentation-profile/v1alpha1";

/// Registered schema catalogue name for [`InstrumentationProfile`].
pub const SCHEMA_NAME: &str = "instrumentation-profile-v1alpha1";

/// Provenance of an instrumentation profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Profile bundled and maintained with AW.
    Builtin,
    /// User- or operator-supplied profile requiring per-target code fingerprints.
    Custom,
}

impl Origin {
    /// Canonical lowercase wire identifier for this origin.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Custom => "custom",
        }
    }
}

/// Target language runtime of an instrumentation profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Runtime {
    /// Python runtime instrumentation (`SourceKind::Python`).
    Python,
    /// JavaScript or TypeScript runtime instrumentation (`SourceKind::Js`).
    Js,
}

impl Runtime {
    /// Canonical lowercase wire identifier for this runtime.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Js => "js",
        }
    }

    /// Maps this profile runtime to the corresponding [`SourceKind`].
    pub fn source_kind(self) -> SourceKind {
        match self {
            Self::Python => SourceKind::Python,
            Self::Js => SourceKind::Js,
        }
    }
}

/// Calling convention of an instrumented target function or method.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallShape {
    /// Synchronous function returning a single value.
    Sync,
    /// Asynchronous function returning a coroutine, future, or promise.
    Async,
    /// Synchronous iterator or generator yielding items over time.
    SyncIter,
    /// Asynchronous iterator or generator yielding items over time.
    AsyncIter,
}

impl CallShape {
    /// Canonical lowercase wire identifier for this call shape.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sync => "sync",
            Self::Async => "async",
            Self::SyncIter => "sync_iter",
            Self::AsyncIter => "async_iter",
        }
    }

    /// Returns whether this call shape yields a stream of items.
    pub fn is_iter(self) -> bool {
        matches!(self, Self::SyncIter | Self::AsyncIter)
    }
}

/// Serialization codec used to read or write an extracted Hook field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    /// Plain UTF-8 text representation.
    Text,
    /// Structured JSON value.
    Json,
    /// JSON payload encoded inside a string.
    JsonString,
}

impl Codec {
    /// Canonical lowercase wire identifier for this codec.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
            Self::JsonString => "json_string",
        }
    }
}

/// Target package name and bounded version comparator range.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PackageSpec {
    pub(super) name: String,
    pub(super) versions: String,
}

impl PackageSpec {
    /// Target package name in the profile runtime's ecosystem.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Comma-separated comparator range restricting compatible package versions.
    pub fn versions(&self) -> &str {
        &self.versions
    }
}

/// Bounded streaming configuration for iterator targets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StreamConfig {
    pub(super) buffer: bool,
    pub(super) max_items: u32,
    pub(super) max_bytes: u32,
}

impl StreamConfig {
    /// Whether the wrapper buffers the full stream before releasing items.
    pub fn buffer(&self) -> bool {
        self.buffer
    }

    /// Maximum number of stream items accepted by the bounded wrapper.
    pub fn max_items(&self) -> u32 {
        self.max_items
    }

    /// Maximum total bytes buffered or observed across the stream.
    pub fn max_bytes(&self) -> u32 {
        self.max_bytes
    }
}

/// Source mapping for one semantic Hook field on an [`InstrumentationTarget`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum FieldBinding {
    /// Extract a field value from a target call argument, receiver, or return path.
    From {
        /// Bounded path expression rooted at `receiver`, `arg.<n>`, `kwarg.<ident>`, or `result`.
        from: String,
        /// Codec describing how the extracted value is encoded.
        codec: Codec,
    },
    /// Bind a fixed constant string (permitted only for `tool_name`).
    Const {
        /// Constant non-blank profile string value.
        #[serde(rename = "const")]
        value: String,
    },
}

impl FieldBinding {
    /// Returns the extraction path when this binding reads from a call value.
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::From { from, .. } => Some(from.as_str()),
            Self::Const { .. } => None,
        }
    }

    /// Returns the codec when this binding reads from a call value.
    pub fn codec(&self) -> Option<Codec> {
        match self {
            Self::From { codec, .. } => Some(*codec),
            Self::Const { .. } => None,
        }
    }

    /// Returns the fixed constant value when this is a `const` binding.
    pub fn const_value(&self) -> Option<&str> {
        match self {
            Self::Const { value } => Some(value.as_str()),
            Self::From { .. } => None,
        }
    }
}

/// One wrappable function or method seam declared by an [`InstrumentationProfile`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InstrumentationTarget {
    pub(super) id: String,
    pub(super) module: String,
    pub(super) qualname: String,
    pub(super) call: CallShape,
    pub(super) hooks: BTreeMap<String, HookPointCapability>,
    pub(super) fields: BTreeMap<String, FieldBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) code_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) stream: Option<StreamConfig>,
}

impl InstrumentationTarget {
    /// Unique identifier for this target within its profile.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Runtime module or package specifier owning the target callable.
    pub fn module(&self) -> &str {
        &self.module
    }

    /// Dotted qualified name of the target function or method within `module`.
    pub fn qualname(&self) -> &str {
        &self.qualname
    }

    /// Calling convention (`sync`, `async`, `sync_iter`, or `async_iter`).
    pub fn call(&self) -> CallShape {
        self.call
    }

    /// Semantic Hook capabilities served by wrapping this target.
    pub fn hooks(&self) -> &BTreeMap<String, HookPointCapability> {
        &self.hooks
    }

    /// Looks up the declared capability for one semantic Hook event on this target.
    pub fn hook(&self, event: &str) -> Option<&HookPointCapability> {
        self.hooks.get(event)
    }

    /// Field extraction or constant bindings declared for this target's Hooks.
    pub fn fields(&self) -> &BTreeMap<String, FieldBinding> {
        &self.fields
    }

    /// Looks up one field binding by canonical field name.
    pub fn field(&self, name: &str) -> Option<&FieldBinding> {
        self.fields.get(name)
    }

    /// Optional lowercase SHA-256 fingerprint of the target seam (required for custom profiles).
    pub fn code_sha256(&self) -> Option<&str> {
        self.code_sha256.as_deref()
    }

    /// Optional bounded streaming configuration for iterator call shapes.
    pub fn stream(&self) -> Option<&StreamConfig> {
        self.stream.as_ref()
    }
}

/// Versioned, side-effect-free instrumentation profile contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct InstrumentationProfile(pub(super) Fields);

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct Fields {
    pub(super) version: String,
    pub(super) id: String,
    pub(super) origin: Origin,
    pub(super) runtime: Runtime,
    pub(super) package: PackageSpec,
    pub(super) targets: Vec<InstrumentationTarget>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFields {
    version: String,
    id: String,
    origin: Origin,
    runtime: Runtime,
    package: RawPackageSpec,
    targets: Vec<RawTarget>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPackageSpec {
    name: String,
    versions: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStreamConfig {
    buffer: bool,
    max_items: u32,
    max_bytes: u32,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawFieldBinding {
    From(RawFromBinding),
    Const(RawConstBinding),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFromBinding {
    from: String,
    codec: Codec,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConstBinding {
    #[serde(rename = "const")]
    value: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTarget {
    id: String,
    module: String,
    qualname: String,
    call: CallShape,
    hooks: BTreeMap<String, HookPointCapability>,
    fields: BTreeMap<String, RawFieldBinding>,
    #[serde(default)]
    code_sha256: Option<String>,
    #[serde(default)]
    stream: Option<RawStreamConfig>,
}

impl From<RawFields> for Fields {
    fn from(raw: RawFields) -> Self {
        Self {
            version: raw.version,
            id: raw.id,
            origin: raw.origin,
            runtime: raw.runtime,
            package: PackageSpec {
                name: raw.package.name,
                versions: raw.package.versions,
            },
            targets: raw
                .targets
                .into_iter()
                .map(|target| InstrumentationTarget {
                    id: target.id,
                    module: target.module,
                    qualname: target.qualname,
                    call: target.call,
                    hooks: target.hooks,
                    fields: target
                        .fields
                        .into_iter()
                        .map(|(name, binding)| {
                            let converted = match binding {
                                RawFieldBinding::From(b) => FieldBinding::From {
                                    from: b.from,
                                    codec: b.codec,
                                },
                                RawFieldBinding::Const(b) => FieldBinding::Const { value: b.value },
                            };
                            (name, converted)
                        })
                        .collect(),
                    code_sha256: target.code_sha256,
                    stream: target.stream.map(|s| StreamConfig {
                        buffer: s.buffer,
                        max_items: s.max_items,
                        max_bytes: s.max_bytes,
                    }),
                })
                .collect(),
        }
    }
}

impl InstrumentationProfile {
    /// Parses bounded JSON bytes and validates schema and semantic invariants.
    ///
    /// # Errors
    /// Rejects oversized documents, duplicate keys, unknown fields, unsupported
    /// versions, invalid package/module/qualname/path grammars, missing required
    /// fields, dishonest streaming configurations, and custom profiles lacking
    /// per-target code fingerprints.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let value = canonical::parse(bytes)?;
        shared_registry()?.validate_instrumentation_profile(&value)
    }

    /// Validates a decoded JSON value as an [`InstrumentationProfile`].
    ///
    /// # Errors
    /// Returns a contract error if the value violates the schema or semantic rules.
    pub fn from_value(value: Value) -> Result<Self, Error> {
        shared_registry()?.validate_instrumentation_profile(&value)
    }

    /// Exact contract version string (`aw-instrumentation-profile/v1alpha1`).
    pub fn version(&self) -> &str {
        &self.0.version
    }

    /// Unique profile identifier used as `source_id` and `contract` when compiled.
    pub fn id(&self) -> &str {
        &self.0.id
    }

    /// Profile provenance (`builtin` or `custom`).
    pub fn origin(&self) -> Origin {
        self.0.origin
    }

    /// Target language runtime (`python` or `js`).
    pub fn runtime(&self) -> Runtime {
        self.0.runtime
    }

    /// Target package name and compatible version comparator range.
    pub fn package(&self) -> &PackageSpec {
        &self.0.package
    }

    /// Ordered instrumentation targets declared by this profile.
    pub fn targets(&self) -> &[InstrumentationTarget] {
        &self.0.targets
    }

    /// Re-verifies all semantic invariants on this profile.
    ///
    /// # Errors
    /// Rejects any violation of profile validation rules R1 through R15.
    pub fn validate(&self) -> Result<(), Error> {
        validate::validate_profile(&self.0)
    }

    /// Encodes this profile into canonical AW JSON bytes.
    ///
    /// # Errors
    /// Returns [`Error::InvalidDocument`] if serialization fails canonical bounds.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, Error> {
        let value = serde_json::to_value(self).map_err(|_| Error::InvalidDocument)?;
        canonical::bytes(&value)
    }

    /// Compiles this profile into a validated [`SourceCapability`] for offline resolution.
    ///
    /// When multiple targets declare the same Hook event, their capabilities are
    /// merged conservatively: a Source may claim an effect or failure action only
    /// if every wrapped path for that event can enforce it. `mode` is `control`
    /// only when every target declaring the event is `control`; otherwise the
    /// merged event downgrades to `observe` and drops control effects and control
    /// failure actions.
    ///
    /// # Errors
    /// Rejects invalid profile invariants, an event whose merged effects lose
    /// `observe`, or invalid `adapter` / `entrypoint` strings.
    pub fn source_capability(
        &self,
        adapter: &str,
        entrypoint: &str,
    ) -> Result<SourceCapability, Error> {
        self.validate()?;
        let kind = self.0.runtime.source_kind();
        let mut by_event: BTreeMap<&str, Vec<&HookPointCapability>> = BTreeMap::new();
        for target in &self.0.targets {
            for (event, cap) in &target.hooks {
                by_event.entry(event.as_str()).or_default().push(cap);
            }
        }

        let mut merged_hooks = BTreeMap::new();
        for (event, caps) in by_event {
            let mut caps_iter = caps.iter();
            let first = caps_iter.next().ok_or(Error::Invariant(
                "event group in instrumentation profile is empty",
            ))?;
            let mode = if caps.iter().all(|c| c.mode == HookAuthority::Control) {
                HookAuthority::Control
            } else {
                HookAuthority::Observe
            };

            let mut effects: BTreeSet<HookEffect> = first.effects.iter().copied().collect();
            let mut failure_actions: BTreeSet<FailureAction> =
                first.failure_actions.iter().copied().collect();
            for cap in caps_iter {
                effects.retain(|effect| cap.effects.contains(effect));
                failure_actions.retain(|action| cap.failure_actions.contains(action));
            }
            if mode == HookAuthority::Observe {
                effects.retain(|effect| !effect.is_control());
                failure_actions.retain(|action| !action.is_control());
            }
            require(
                effects.contains(&HookEffect::Observe),
                "merged Hook capability lost observe effect",
            )?;
            merged_hooks.insert(
                event.to_owned(),
                HookPointCapability {
                    post_permission: false,
                    mode,
                    effects: effects.into_iter().collect(),
                    failure_actions: failure_actions.into_iter().collect(),
                },
            );
        }

        let capability = SourceCapability::new(
            &self.0.id,
            kind,
            adapter,
            &self.0.id,
            entrypoint,
            merged_hooks,
        )?;
        capability.validate()?;
        Ok(capability)
    }
}

impl Registry {
    /// Validates a JSON value against `instrumentation-profile-v1alpha1` and semantic rules.
    ///
    /// # Errors
    /// Rejects schema mismatches, invalid runtime/package/module/qualname/path
    /// grammars, missing required fields, dishonest streaming declarations, and
    /// missing custom code fingerprints.
    pub fn validate_instrumentation_profile(
        &self,
        value: &Value,
    ) -> Result<InstrumentationProfile, Error> {
        self.validate(SCHEMA_NAME, value)?;
        let raw: RawFields = serde_json::from_value(value.clone())
            .map_err(|_| Error::SchemaMismatch(SCHEMA_NAME.into()))?;
        let profile = InstrumentationProfile(raw.into());
        profile.validate()?;
        Ok(profile)
    }
}
