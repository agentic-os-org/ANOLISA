//! Errors raised while loading, validating, and compiling the OS-adapter rule
//! artifact. Every variant is surfaced before the FUSE mount begins so the
//! operator gets an actionable message rather than a silently disabled adapter.

use std::path::PathBuf;

/// Errors raised while loading or validating the OS-adapter rule artifact.
#[derive(Debug, thiserror::Error)]
pub enum OsAdapterError {
    /// `target_os` was configured with an unsupported value.
    ///
    /// The stage builder rejects it fail-closed so a bad value can never be
    /// silently coerced to `auto` (and then resolved from `/etc/os-release`),
    /// even when a caller bypasses the higher-level config validation.
    #[error("os_adapter: invalid target_os '{value}'; allowed: auto, ubuntu, alinux")]
    InvalidTargetSelector { value: String },

    /// `rules_path` was present in the config but blank/whitespace.
    ///
    /// A blank override is a configuration mistake, not a request for the
    /// built-in catalog, so the stage builder rejects it fail-closed even when
    /// a caller bypasses the higher-level config validation.
    #[error(
        "os_adapter: configured rules_path is blank; omit it to use the built-in \
         catalog or set a non-empty path"
    )]
    BlankRulesPath,

    /// The rule file could not be read.
    #[error("os_adapter: cannot read rules file '{path}': {source}")]
    ReadRules {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The rule file is not valid YAML or does not match the expected schema.
    #[error("os_adapter: invalid rules YAML in '{path}': {source}")]
    Yaml {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },

    /// The rule file parsed but contained no rules.
    #[error("os_adapter: rules file '{path}' contains no rules")]
    EmptyRules { path: PathBuf },

    /// A rule omitted the required `auto_apply` eligibility field.
    ///
    /// Emitted for legacy provider artifacts that predate the explicit
    /// eligibility contract; the operator must add `auto_apply: always|never`
    /// to every rule.
    #[error(
        "os_adapter: rule {index}: missing required field 'auto_apply' (allowed: always, never); \
         legacy artifacts without explicit eligibility are rejected"
    )]
    MissingAutoApply { index: usize },

    /// A rule declared an unsupported `direction`.
    #[error(
        "os_adapter: rule {index}: invalid direction '{value}'; \
         allowed: bidirectional, ubuntu_to_alinux_only, alinux_to_ubuntu_only"
    )]
    InvalidDirection { index: usize, value: String },

    /// A rule declared unsupported `auto_apply` metadata.
    #[error("os_adapter: rule {index}: unsupported auto_apply '{value}'; allowed: always, never")]
    InvalidAutoApply { index: usize, value: String },

    /// A rule declared an unsupported matching mode.
    #[error("os_adapter: rule {index}: unsupported match '{value}'; allowed: literal, token")]
    InvalidMatchMode { index: usize, value: String },

    /// A rule declared an empty source or target pattern.
    #[error("os_adapter: rule {index}: '{field}' must not be empty")]
    EmptyPattern { index: usize, field: &'static str },

    /// Two rules define the same source pattern with the same target.
    #[error("os_adapter: duplicate rule for source pattern '{pattern}' (target OS {target})")]
    DuplicateRule {
        pattern: String,
        target: &'static str,
    },

    /// Two rules define the same source pattern with conflicting targets.
    #[error(
        "os_adapter: ambiguous rules for source pattern '{pattern}' (target OS {target}): \
         '{existing}' vs '{conflicting}'"
    )]
    AmbiguousRule {
        pattern: String,
        target: &'static str,
        existing: String,
        conflicting: String,
    },

    /// `target_os = auto` but `/etc/os-release` could not be read.
    #[error("os_adapter: cannot read /etc/os-release for target_os=auto: {source}")]
    ReadOsRelease {
        #[source]
        source: std::io::Error,
    },

    /// `target_os = auto` but the running distribution is not Ubuntu/Debian or
    /// Alinux/Anolis by exact `/etc/os-release` `ID`.
    #[error(
        "os_adapter: target_os=auto could not map /etc/os-release ID to a supported \
         target (expected ID=ubuntu/debian or ID=alinux/anolis); \
         set target_os = \"ubuntu\" or \"alinux\" explicitly on other distributions"
    )]
    UnknownOs,
}
