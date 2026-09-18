//! Environment-variable capability view, resolved entirely inside the CLI.
//!
//! The view answers "what will the hooks in *this* environment do", so it reads
//! only variables inherited by the CLI process. It never reads Agent config
//! files or home directories, and it never proves that a hook is loaded in a
//! target Agent process. The daemon is not involved.
//!
//! Migration contract, the two gaps that a future capability migration must
//! close (G1, G4) and the items that follow from V1's design, the rolling
//! migration itself or an accepted trade-off (G2, G3, G5, G6, G7, G8) are
//! recorded in `docs/design/V2_CAPABILITY_VIEW_MIGRATION_zh.md`.

pub(crate) mod manifest;
pub(crate) mod render;
pub(crate) mod resolve;

use std::fmt::{self, Write as _};

use serde::Serialize;
use serde::ser::{SerializeMap, SerializeStruct as _, Serializer};

use manifest::{AGENTS, CANONICAL_CAPABILITIES};
pub use resolve::Environment;
use resolve::{EnvValue, ResolvedEnv};

/// Reads the variables this view can report from the current process.
///
/// `std::env::vars` panics on a value that is not valid UTF-8, which would let
/// an unrelated variable both break a read-only view and push its own value
/// into the panic message on stderr. Reading through `vars_os` and keeping only
/// the manifest names avoids both.
///
/// A value that is not valid UTF-8 is decoded the way `CPython` decodes the
/// environment on Unix: every undecodable byte becomes the escape text V1 would
/// print for its surrogate. The value therefore stays invalid for every parsed
/// variable (falling back to the documented default with a diagnostic) and is
/// reported identically to V1 for the one variable echoed close to verbatim.
#[must_use]
pub fn process_environment() -> Environment {
    let names = manifest::env_names();
    std::env::vars_os()
        .filter_map(|(name, value)| {
            let name = name.into_string().ok()?;
            names
                .contains(&name.as_str())
                .then(|| (name, decode_value(&value)))
        })
        .collect()
}

/// Decodes an environment value, escaping bytes `CPython` would surrogate-escape.
#[cfg(unix)]
fn decode_value(value: &std::ffi::OsStr) -> String {
    use std::os::unix::ffi::OsStrExt as _;

    let mut bytes = value.as_bytes();
    let mut text = String::new();
    loop {
        match std::str::from_utf8(bytes) {
            Ok(valid) => {
                text.push_str(valid);
                return text;
            }
            Err(error) => {
                let (valid, rest) = bytes.split_at(error.valid_up_to());
                text.push_str(&String::from_utf8_lossy(valid));
                // `error_len` is `None` when the value ends mid-sequence, in
                // which case every remaining byte is undecodable as well.
                let undecodable = error.error_len().unwrap_or(rest.len());
                for byte in &rest[..undecodable] {
                    // CPython maps byte `b` to U+DC00 + b, and V1 prints that
                    // surrogate as `\udcXX`.
                    let _ = write!(text, "\\udc{byte:02x}");
                }
                bytes = &rest[undecodable..];
            }
        }
    }
}

/// Decodes an environment value on platforms without byte-oriented values.
#[cfg(not(unix))]
fn decode_value(value: &std::ffi::OsStr) -> String {
    value.to_string_lossy().into_owned()
}

/// Resolved configuration of one agent/capability pair.
#[derive(Debug)]
pub struct CapabilityRecord {
    agent: &'static str,
    capability: &'static str,
    enabled: bool,
    mode: String,
    scan_mode: String,
    timeout: String,
    env: Vec<ResolvedEnv>,
    diagnostics: Vec<String>,
}

impl CapabilityRecord {
    fn new(agent: &'static str, capability: &'static str, env: &Environment) -> Self {
        let spec = manifest::spec(agent, capability);
        let (values, diagnostics) = resolve::resolve(spec.env, env);
        Self {
            agent,
            capability,
            enabled: resolve::enabled(&values),
            mode: resolve::mode(&values, spec.default_mode),
            scan_mode: resolve::scan_mode(capability, &values),
            timeout: resolve::timeout(agent, capability, &values),
            env: values,
            diagnostics,
        }
    }

    pub(crate) fn agent(&self) -> &'static str {
        self.agent
    }

    pub(crate) fn capability(&self) -> &'static str {
        self.capability
    }

    pub(crate) fn enabled_label(&self) -> &'static str {
        if self.enabled { "enabled" } else { "disabled" }
    }

    pub(crate) fn mode(&self) -> &str {
        &self.mode
    }

    pub(crate) fn scan_mode(&self) -> &str {
        &self.scan_mode
    }

    pub(crate) fn timeout(&self) -> &str {
        &self.timeout
    }

    pub(crate) fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }
}

impl Serialize for CapabilityRecord {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut record = serializer.serialize_struct("CapabilityRecord", 8)?;
        record.serialize_field("agent", self.agent)?;
        record.serialize_field("capability", self.capability)?;
        record.serialize_field("enabled", self.enabled_label())?;
        record.serialize_field("mode", &self.mode)?;
        record.serialize_field("scan_mode", &self.scan_mode)?;
        record.serialize_field("timeout", &self.timeout)?;
        record.serialize_field("env", &EnvProjection(&self.env))?;
        record.serialize_field("diagnostics", &self.diagnostics)?;
        record.end()
    }
}

/// Serializes the resolved variables in manifest order, without the raw value.
///
/// Raw values are deliberately dropped: they may carry operator-supplied text,
/// and the effective/default pair is what explains hook behaviour.
struct EnvProjection<'a>(&'a [ResolvedEnv]);

impl Serialize for EnvProjection<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // serde_json writes map entries in iteration order, so manifest order
        // survives without enabling the `preserve_order` feature globally.
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for value in self.0 {
            map.serialize_entry(
                value.name,
                &EnvEntry {
                    effective: &value.effective,
                    default: &value.default,
                },
            )?;
        }
        map.end()
    }
}

#[derive(Serialize)]
struct EnvEntry<'a> {
    effective: &'a EnvValue,
    default: &'a EnvValue,
}

impl Serialize for EnvValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Bool(value) => serializer.serialize_bool(*value),
            Self::Text(value) => serializer.serialize_str(value),
        }
    }
}

/// Rejected `--agent` or `--capability` value, reported without a raw echo.
#[derive(Debug, PartialEq, Eq)]
pub struct FilterError {
    field: &'static str,
    value: String,
    allowed: &'static str,
}

impl fmt::Display for FilterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unknown {}: {}. Allowed values: {}",
            self.field, self.value, self.allowed
        )
    }
}

/// Normalizes one filter value, or reports the allowed set.
///
/// # Errors
/// Returns [`FilterError`] when the trimmed, lowercased value is not allowed.
fn normalize(
    value: Option<&str>,
    allowed: &[&'static str],
    field: &'static str,
    allowed_text: &'static str,
) -> Result<Option<&'static str>, FilterError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let normalized = value.trim().to_lowercase();
    allowed
        .iter()
        .find(|candidate| **candidate == normalized)
        .copied()
        .map(Some)
        .ok_or_else(|| FilterError {
            field,
            value: resolve::safe_value(value),
            allowed: allowed_text,
        })
}

const AGENT_LIST: &str = "qoder, qwen, codex, cosh, openclaw, hermes";
const CAPABILITY_LIST: &str = "code-scan, prompt-scan, pii-check, skill-ledger, observability";

/// Builds the capability records selected by the optional filters.
///
/// Records are ordered by agent then capability so two runs of the same
/// environment produce byte-identical output.
///
/// # Errors
/// Returns [`FilterError`] for an unknown agent or capability name.
pub fn query(
    env: &Environment,
    agent: Option<&str>,
    capability: Option<&str>,
) -> Result<Vec<CapabilityRecord>, FilterError> {
    let agent = normalize(agent, &AGENTS, "agent", AGENT_LIST)?;
    let capability = normalize(
        capability,
        &CANONICAL_CAPABILITIES,
        "capability",
        CAPABILITY_LIST,
    )?;
    let mut agents: Vec<&'static str> = AGENTS
        .into_iter()
        .filter(|name| agent.is_none_or(|selected| selected == *name))
        .collect();
    agents.sort_unstable();
    let mut capabilities: Vec<&'static str> = CANONICAL_CAPABILITIES
        .into_iter()
        .filter(|name| capability.is_none_or(|selected| selected == *name))
        .collect();
    capabilities.sort_unstable();
    let mut records = Vec::with_capacity(agents.len() * capabilities.len());
    for agent in agents {
        for capability in &capabilities {
            records.push(CapabilityRecord::new(agent, capability, env));
        }
    }
    Ok(records)
}

/// Serializes the records as the two-space-indented JSON array V1 emits.
///
/// # Errors
/// Returns a serialization failure, which the record shapes make unreachable in
/// practice.
pub fn render_json(records: &[CapabilityRecord]) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(records)
}

/// Renders the per-agent table used by the default output format.
pub fn render_table(records: &[CapabilityRecord]) -> String {
    render::table(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty() -> Environment {
        Environment::new()
    }

    #[test]
    fn default_query_lists_every_pair_in_stable_order() {
        let records = query(&empty(), None, None).expect("no filters");
        assert_eq!(records.len(), AGENTS.len() * CANONICAL_CAPABILITIES.len());
        let pairs: Vec<(&str, &str)> = records
            .iter()
            .map(|record| (record.agent, record.capability))
            .collect();
        let mut expected = pairs.clone();
        expected.sort_unstable();
        assert_eq!(pairs, expected);
        assert!(records.iter().all(|record| record.enabled));
        assert!(records.iter().all(|record| record.diagnostics.is_empty()));
    }

    #[test]
    fn filters_trim_and_normalize_case() {
        let records = query(&empty(), Some(" QODER "), Some(" PROMPT-SCAN ")).expect("filters");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].agent, "qoder");
        assert_eq!(records[0].capability, "prompt-scan");
    }

    #[test]
    fn hook_level_capability_aliases_are_rejected() {
        for alias in ["scan-code", "scan-prompt", "code_scan"] {
            let error = query(&empty(), None, Some(alias)).expect_err("alias is not canonical");
            assert_eq!(
                error.to_string(),
                format!("unknown capability: {alias}. Allowed values: {CAPABILITY_LIST}")
            );
        }
        let error = query(&empty(), Some("unsupported"), None).expect_err("unknown agent");
        assert_eq!(
            error.to_string(),
            format!("unknown agent: unsupported. Allowed values: {AGENT_LIST}")
        );
    }

    #[test]
    fn rejected_filter_values_are_escaped_before_they_reach_output() {
        let error = query(&empty(), Some("qoder\u{1}"), None).expect_err("unknown agent");
        assert!(error.to_string().contains("qoder\\x01"), "{error}");
        assert!(!error.to_string().contains('\u{1}'), "{error}");
    }

    #[test]
    fn json_projection_hides_raw_values_and_keeps_manifest_order() {
        let mut env = empty();
        env.insert("CODE_SCANNER_HOOK_ENABLED".to_owned(), "false".to_owned());
        let records = query(&env, Some("qoder"), Some("code-scan")).expect("filters");
        let json = render_json(&records).expect("records serialize");
        assert!(!json.contains("\"raw\""), "{json}");
        let enabled_at = json
            .find("CODE_SCANNER_HOOK_ENABLED")
            .expect("switch present");
        let mode_at = json.find("CODE_SCANNER_MODE").expect("mode present");
        let timeout_at = json.find("CODE_SCANNER_TIMEOUT").expect("timeout present");
        assert!(enabled_at < mode_at && mode_at < timeout_at, "{json}");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(value[0]["enabled"], "disabled");
        assert_eq!(value[0]["timeout"], "10");
        assert_eq!(value[0]["scan_mode"], "-");
        assert_eq!(
            value[0]["env"]["CODE_SCANNER_HOOK_ENABLED"]["effective"],
            false
        );
        assert_eq!(
            value[0]["env"]["CODE_SCANNER_HOOK_ENABLED"]["default"],
            true
        );
    }

    #[test]
    fn json_records_expose_exactly_the_v1_field_set() {
        let records = query(&empty(), Some("cosh"), None).expect("filters");
        let json = render_json(&records).expect("records serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        for record in value.as_array().expect("array") {
            let mut keys: Vec<&str> = record
                .as_object()
                .expect("object")
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            let mut expected = vec![
                "agent",
                "capability",
                "enabled",
                "mode",
                "scan_mode",
                "timeout",
                "env",
                "diagnostics",
            ];
            expected.sort_unstable();
            assert_eq!(keys, expected);
        }
        // `serde_json::Value` re-sorts object keys, so field order is asserted
        // on the emitted text instead.
        let first = json.find("\"agent\"").expect("agent first");
        let positions: Vec<usize> = [
            "\"agent\"",
            "\"capability\"",
            "\"enabled\"",
            "\"mode\"",
            "\"scan_mode\"",
            "\"timeout\"",
            "\"env\"",
            "\"diagnostics\"",
        ]
        .iter()
        .map(|key| json[first..].find(key).expect("key present"))
        .collect();
        let mut sorted = positions.clone();
        sorted.sort_unstable();
        assert_eq!(positions, sorted, "{json}");
    }
}
