//! Environment-variable resolution shared by every agent/capability pair.
//!
//! Every function takes the environment explicitly so the view can be tested
//! without mutating the process environment; only `main` reads `std::env`.

use std::collections::HashMap;
use std::fmt::Write as _;

use unicode_properties::{GeneralCategoryGroup, UnicodeGeneralCategory as _};

use super::manifest::{self, EnvKind, EnvSpec};

/// Process environment as seen by the CLI, keyed by variable name.
pub type Environment = HashMap<String, String>;

/// Escape limit for the one value reported close to verbatim; see `safe_value`.
const VALUE_LIMIT: usize = 80;

/// Effective or default value of one variable, preserving the V1 JSON types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvValue {
    /// Enable switches serialize as JSON booleans.
    Bool(bool),
    /// Modes, timeouts and paths serialize as JSON strings.
    Text(String),
}

impl EnvValue {
    fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }

    /// Returns the string form, or an empty string for a boolean.
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Bool(_) => "",
            Self::Text(text) => text,
        }
    }

    /// Renders the value the way Python `repr` does, for fallback diagnostics.
    fn repr(&self) -> String {
        match self {
            Self::Bool(true) => "True".to_owned(),
            Self::Bool(false) => "False".to_owned(),
            Self::Text(text) => format!("'{text}'"),
        }
    }
}

/// One resolved variable: what the environment said and what the hook will use.
#[derive(Debug, Clone)]
pub struct ResolvedEnv {
    pub(crate) name: &'static str,
    /// Kept out of every rendered output; only the precedence rules read it.
    raw: Option<String>,
    pub(crate) effective: EnvValue,
    pub(crate) default: EnvValue,
}

/// Resolves one capability's variables into values plus operator diagnostics.
pub(crate) fn resolve(specs: &[EnvSpec], env: &Environment) -> (Vec<ResolvedEnv>, Vec<String>) {
    let mut values = Vec::with_capacity(specs.len());
    let mut diagnostics = Vec::new();
    for spec in specs {
        let raw = env.get(spec.name).cloned();
        let (effective, default) = resolve_one(spec, raw.as_deref(), &mut diagnostics);
        values.push(ResolvedEnv {
            name: spec.name,
            raw,
            effective,
            default,
        });
    }
    (values, diagnostics)
}

fn resolve_one(
    spec: &EnvSpec,
    raw: Option<&str>,
    diagnostics: &mut Vec<String>,
) -> (EnvValue, EnvValue) {
    match spec.kind {
        EnvKind::StrictBool(default) => (
            resolve_bool(spec.name, raw, default, true, diagnostics),
            EnvValue::Bool(default),
        ),
        EnvKind::BroadBool(default) => (
            resolve_bool(spec.name, raw, default, false, diagnostics),
            EnvValue::Bool(default),
        ),
        EnvKind::Keyword {
            default,
            allowed,
            aliased,
        } => (
            resolve_keyword(spec.name, raw, default, allowed, aliased, diagnostics),
            EnvValue::text(default),
        ),
        EnvKind::IntTimeout {
            default,
            max,
            require_positive,
        } => (
            resolve_int_timeout(spec.name, raw, default, max, require_positive, diagnostics),
            EnvValue::text(default),
        ),
        EnvKind::FloatTimeout { default, max } => (
            resolve_float_timeout(spec.name, raw, default, max, diagnostics),
            EnvValue::text(default),
        ),
        // Gap G1: V2 has no prompt-scanner engine to query, so the default is
        // empty and no backend is known. This is the same degraded path V1
        // takes before its native extension is built: the configured value is
        // still reported, but an unsupported backend cannot be flagged.
        EnvKind::Identifier => (
            raw.map(str::trim)
                .filter(|text| !text.is_empty())
                .map_or_else(
                    || EnvValue::text(""),
                    |text| EnvValue::Text(safe_value(text)),
                ),
            EnvValue::text(""),
        ),
        EnvKind::DataHome { default } => (
            resolve_data_home(spec.name, raw, default, diagnostics),
            EnvValue::text(default),
        ),
    }
}

fn resolve_bool(
    name: &str,
    raw: Option<&str>,
    default: bool,
    strict: bool,
    diagnostics: &mut Vec<String>,
) -> EnvValue {
    let Some(raw) = raw else {
        return EnvValue::Bool(default);
    };
    let normalized = raw.trim().to_lowercase();
    let resolved = if strict {
        match normalized.as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    } else {
        match normalized.as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        }
    };
    if let Some(value) = resolved {
        return EnvValue::Bool(value);
    }
    diagnostics.push(fallback(name, &EnvValue::Bool(default)));
    EnvValue::Bool(default)
}

fn resolve_keyword(
    name: &str,
    raw: Option<&str>,
    default: &str,
    allowed: &[&str],
    aliased: bool,
    diagnostics: &mut Vec<String>,
) -> EnvValue {
    let Some(raw) = raw else {
        return EnvValue::text(default);
    };
    let normalized = raw.trim().to_lowercase();
    let normalized = if aliased {
        match normalized.as_str() {
            "debug" => "observe".to_owned(),
            "deny" => "block".to_owned(),
            _ => normalized,
        }
    } else {
        normalized
    };
    if allowed.contains(&normalized.as_str()) {
        return EnvValue::Text(normalized);
    }
    diagnostics.push(fallback(name, &EnvValue::text(default)));
    EnvValue::text(default)
}

fn resolve_int_timeout(
    name: &str,
    raw: Option<&str>,
    default: &str,
    max: Option<i64>,
    require_positive: bool,
    diagnostics: &mut Vec<String>,
) -> EnvValue {
    let Some(raw) = raw else {
        return EnvValue::text(default);
    };
    let Ok(mut value) = raw.trim().parse::<i64>() else {
        diagnostics.push(fallback(name, &EnvValue::text(default)));
        return EnvValue::text(default);
    };
    if value <= 0 {
        if require_positive {
            diagnostics.push(fallback(name, &EnvValue::text(default)));
            return EnvValue::text(default);
        }
        // A nonpositive timeout is passed through because the hook does the
        // same; the operator needs to know the subprocess may fail open.
        diagnostics.push(format!(
            "{name} is nonpositive; the hook subprocess may fail open"
        ));
    }
    if let Some(max) = max {
        value = value.min(max);
    }
    EnvValue::Text(value.to_string())
}

fn resolve_float_timeout(
    name: &str,
    raw: Option<&str>,
    default: &str,
    max: Option<f64>,
    diagnostics: &mut Vec<String>,
) -> EnvValue {
    let Some(raw) = raw else {
        return EnvValue::text(default);
    };
    let parsed = raw.trim().parse::<f64>();
    let Ok(mut value) = parsed else {
        diagnostics.push(fallback(name, &EnvValue::text(default)));
        return EnvValue::text(default);
    };
    if !value.is_finite() || value <= 0.0 {
        diagnostics.push(fallback(name, &EnvValue::text(default)));
        return EnvValue::text(default);
    }
    if let Some(max) = max {
        value = value.min(max);
    }
    EnvValue::Text(format_number(value))
}

fn resolve_data_home(
    name: &str,
    raw: Option<&str>,
    default: &str,
    diagnostics: &mut Vec<String>,
) -> EnvValue {
    let Some(raw) = raw else {
        return EnvValue::text(default);
    };
    if valid_data_home(raw) {
        return EnvValue::text(raw);
    }
    if !raw.is_empty() {
        diagnostics.push(fallback(name, &EnvValue::text(default)));
    }
    EnvValue::text(default)
}

/// Matches ANOLISA's absolute data-root syntax without resolving the filesystem.
///
/// Gap G4: once skill-ledger migrates, V2 will hold two copies of this check;
/// fold them into one implementation then.
fn valid_data_home(value: &str) -> bool {
    !value.is_empty()
        && value.starts_with('/')
        && !value
            .split('/')
            .any(|segment| segment == "." || segment == "..")
}

fn fallback(name: &str, default: &EnvValue) -> String {
    format!("{name} has an invalid value; using {}", default.repr())
}

fn format_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

/// Escapes non-printable characters and caps the length of a reported value.
///
/// Only one variable reaches output close to verbatim (the L2 backend name), and
/// one more is echoed in the unknown-agent error, so this keeps a hostile
/// environment or argv from reaching a terminal with characters that rewrite how
/// the rest of the line reads.
pub(crate) fn safe_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if printable(character) {
            escaped.push(character);
            continue;
        }
        let code_point = u32::from(character);
        // Writing into a String is infallible; the Result exists for io sinks.
        let _ = if code_point <= 0xff {
            write!(escaped, "\\x{code_point:02x}")
        } else if code_point <= 0xffff {
            write!(escaped, "\\u{code_point:04x}")
        } else {
            write!(escaped, "\\U{code_point:08x}")
        };
    }
    if escaped.chars().count() > VALUE_LIMIT {
        let head: String = escaped.chars().take(VALUE_LIMIT - 1).collect();
        return format!("{head}…");
    }
    escaped
}

/// Mirrors Python's `str.isprintable`, which V1 relies on for the same values.
///
/// The rejected groups are exactly the categories `CPython` rejects: `Other`
/// (`Cc`, `Cf`, `Cs`, `Co`, `Cn`) and `Separator` (`Zs`, `Zl`, `Zp`), with ASCII
/// space kept printable. `Cf` is the reason a general-category table is required
/// rather than `std`'s `is_control`/`is_whitespace` pair: bidirectional overrides
/// (U+202A..U+202E, U+2066..U+2069) and zero-width characters (U+200B..U+200F)
/// are neither control nor whitespace, yet they let an attacker-controlled value
/// reorder or hide what an operator reads in the very output used to audit it.
///
/// Item G6: the rule matches V1, but the two sides carry different Unicode data
/// (this table is 17.0, `CPython` 3.11.6 is 14.0.0), so a code point unassigned
/// in 14.0 and assigned later is escaped by V1 and kept here.
fn printable(character: char) -> bool {
    character == ' '
        || !matches!(
            character.general_category_group(),
            GeneralCategoryGroup::Other | GeneralCategoryGroup::Separator
        )
}

/// Reports whether the hook runs, applying the PII enable-switch precedence.
pub(crate) fn enabled(values: &[ResolvedEnv]) -> bool {
    let hook = find(values, "PII_CHECKER_HOOK_ENABLED");
    let legacy = find(values, "PII_CHECKER_ENABLED");
    if let (Some(hook), Some(legacy)) = (hook, legacy) {
        // The hook-scoped switch wins when it is set at all; the legacy one is
        // only consulted for environments that never migrated.
        let winner = if hook.raw.is_some() { hook } else { legacy };
        return winner.effective != EnvValue::Bool(false);
    }
    !values
        .iter()
        .any(|value| value.name.ends_with("_ENABLED") && value.effective == EnvValue::Bool(false))
}

/// Returns the interaction mode of the capability, or its manifest default.
pub(crate) fn mode(values: &[ResolvedEnv], default: &str) -> String {
    for name in [
        "CODE_SCANNER_MODE",
        "PROMPT_SCANNER_MODE",
        "PII_CHECKER_MODE",
        "SKILL_LEDGER_MODE",
    ] {
        if let Some(value) = find(values, name) {
            return value.effective.as_str().to_owned();
        }
    }
    default.to_owned()
}

/// Returns the prompt-scanner depth, or `-` for capabilities without one.
pub(crate) fn scan_mode(capability: &str, values: &[ResolvedEnv]) -> String {
    if capability != "prompt-scan" {
        return "-".to_owned();
    }
    find(values, "PROMPT_SCANNER_SCAN_MODE").map_or_else(
        || "standard".to_owned(),
        |value| value.effective.as_str().to_owned(),
    )
}

/// Returns the effective timeout, falling back to the hook's built-in value.
pub(crate) fn timeout(agent: &str, capability: &str, values: &[ResolvedEnv]) -> String {
    if let Some(value) = values.iter().find(|value| value.name.ends_with("_TIMEOUT")) {
        let text = value.effective.as_str().trim();
        return if text.is_empty() {
            "-".to_owned()
        } else {
            text.to_owned()
        };
    }
    manifest::static_default_timeout(agent, capability)
        .unwrap_or("-")
        .to_owned()
}

fn find<'a>(values: &'a [ResolvedEnv], name: &str) -> Option<&'a ResolvedEnv> {
    values.iter().find(|value| value.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Environment {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn resolve_pair(
        agent: &str,
        capability: &str,
        pairs: &[(&str, &str)],
    ) -> (Vec<ResolvedEnv>, Vec<String>) {
        resolve(manifest::spec(agent, capability).env, &env(pairs))
    }

    #[test]
    fn strict_enable_switch_accepts_only_true_and_false() {
        for (raw, expected) in [
            ("true", true),
            (" TRUE ", true),
            ("false", false),
            ("False", false),
        ] {
            let (values, diagnostics) =
                resolve_pair("qoder", "code-scan", &[("CODE_SCANNER_HOOK_ENABLED", raw)]);
            assert_eq!(values[0].effective, EnvValue::Bool(expected), "raw={raw}");
            assert!(diagnostics.is_empty(), "raw={raw}");
            assert_eq!(enabled(&values), expected);
        }
        for raw in ["1", "yes", "on", "maybe", ""] {
            let (values, diagnostics) =
                resolve_pair("qoder", "code-scan", &[("CODE_SCANNER_HOOK_ENABLED", raw)]);
            assert_eq!(values[0].effective, EnvValue::Bool(true), "raw={raw}");
            assert_eq!(
                diagnostics,
                vec!["CODE_SCANNER_HOOK_ENABLED has an invalid value; using True"],
                "raw={raw}"
            );
        }
    }

    #[test]
    fn broad_legacy_switch_accepts_the_wider_vocabulary() {
        for (raw, expected) in [
            ("1", true),
            ("yes", true),
            ("on", true),
            ("0", false),
            ("no", false),
            ("off", false),
        ] {
            let (values, diagnostics) =
                resolve_pair("qwen", "pii-check", &[("PII_CHECKER_ENABLED", raw)]);
            assert_eq!(values[1].effective, EnvValue::Bool(expected), "raw={raw}");
            assert!(diagnostics.is_empty(), "raw={raw}");
            assert_eq!(enabled(&values), expected, "raw={raw}");
        }
        let (values, diagnostics) =
            resolve_pair("qwen", "pii-check", &[("PII_CHECKER_ENABLED", "maybe")]);
        assert_eq!(values[1].effective, EnvValue::Bool(true));
        assert_eq!(
            diagnostics,
            vec!["PII_CHECKER_ENABLED has an invalid value; using True"]
        );
    }

    #[test]
    fn hook_scoped_pii_switch_takes_precedence_over_the_legacy_one() {
        for (pairs, expected) in [
            (vec![("PII_CHECKER_ENABLED", "false")], false),
            (
                vec![
                    ("PII_CHECKER_ENABLED", "false"),
                    ("PII_CHECKER_HOOK_ENABLED", "true"),
                ],
                true,
            ),
            (
                vec![
                    ("PII_CHECKER_ENABLED", "true"),
                    ("PII_CHECKER_HOOK_ENABLED", "false"),
                ],
                false,
            ),
        ] {
            let (values, _) = resolve_pair("qwen", "pii-check", &pairs);
            assert_eq!(enabled(&values), expected, "pairs={pairs:?}");
        }
    }

    #[test]
    fn hook_policy_aliases_apply_before_the_agent_allowlist() {
        for (agent, raw, expected, diagnosed) in [
            ("qoder", "debug", "observe", false),
            ("qoder", "deny", "block", false),
            ("qoder", " Ask ", "ask", false),
            ("codex", "ask", "observe", true),
            ("cosh", "block", "ask", true),
            ("hermes", "ask", "observe", true),
        ] {
            let (values, diagnostics) =
                resolve_pair(agent, "code-scan", &[("CODE_SCANNER_MODE", raw)]);
            assert_eq!(mode(&values, "observe"), expected, "{agent}/{raw}");
            assert_eq!(!diagnostics.is_empty(), diagnosed, "{agent}/{raw}");
        }
    }

    #[test]
    fn integer_timeouts_clamp_diagnose_and_pass_through_nonpositive_values() {
        let cases = [
            (
                "qoder",
                "code-scan",
                "CODE_SCANNER_TIMEOUT",
                "30",
                "30",
                None,
            ),
            (
                "qoder",
                "code-scan",
                "CODE_SCANNER_TIMEOUT",
                "0",
                "0",
                Some("CODE_SCANNER_TIMEOUT is nonpositive; the hook subprocess may fail open"),
            ),
            (
                "qoder",
                "code-scan",
                "CODE_SCANNER_TIMEOUT",
                "1.5",
                "10",
                Some("CODE_SCANNER_TIMEOUT has an invalid value; using '10'"),
            ),
            (
                "hermes",
                "observability",
                "OBSERVABILITY_TIMEOUT",
                "999",
                "5",
                None,
            ),
            (
                "openclaw",
                "observability",
                "OBSERVABILITY_TIMEOUT",
                "0",
                "5",
                Some("OBSERVABILITY_TIMEOUT has an invalid value; using '5'"),
            ),
        ];
        for (agent, capability, name, raw, expected, diagnostic) in cases {
            let (values, diagnostics) = resolve_pair(agent, capability, &[(name, raw)]);
            assert_eq!(
                timeout(agent, capability, &values),
                expected,
                "{name}={raw}"
            );
            assert_eq!(
                diagnostics,
                diagnostic
                    .map(ToOwned::to_owned)
                    .into_iter()
                    .collect::<Vec<_>>(),
                "{name}={raw}"
            );
        }
    }

    /// Timeout values use Rust numeric syntax, not Python's.
    ///
    /// V1 parses with Python `int`/`float`, which also accept underscore
    /// separators and non-ASCII decimal digits, and prints small floats in
    /// Python's `repr` form. V2 keeps Rust semantics on purpose: such a value
    /// falls back to the documented default with a visible diagnostic instead
    /// of being silently reinterpreted. This test pins that choice so it is not
    /// mistaken for a defect later.
    #[test]
    fn timeouts_reject_python_only_numeric_syntax() {
        for raw in ["2_0", "\u{ff12}\u{ff10}"] {
            let (values, diagnostics) =
                resolve_pair("qoder", "code-scan", &[("CODE_SCANNER_TIMEOUT", raw)]);
            assert_eq!(timeout("qoder", "code-scan", &values), "10", "raw={raw}");
            assert_eq!(
                diagnostics,
                vec!["CODE_SCANNER_TIMEOUT has an invalid value; using '10'"],
                "raw={raw}"
            );
        }
    }

    /// Small floats keep Rust's decimal form rather than Python's `repr`.
    #[test]
    fn small_float_timeouts_are_reported_in_decimal_form() {
        let (values, diagnostics) =
            resolve_pair("qoder", "skill-ledger", &[("SKILL_LEDGER_TIMEOUT", "1e-7")]);
        assert_eq!(timeout("qoder", "skill-ledger", &values), "0.0000001");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn float_timeouts_keep_fractions_and_clamp_to_the_agent_maximum() {
        let (values, diagnostics) =
            resolve_pair("qoder", "skill-ledger", &[("SKILL_LEDGER_TIMEOUT", "0.05")]);
        assert_eq!(timeout("qoder", "skill-ledger", &values), "0.05");
        assert!(diagnostics.is_empty());

        let (values, diagnostics) =
            resolve_pair("qwen", "pii-check", &[("PII_CHECKER_TIMEOUT", "99")]);
        assert_eq!(timeout("qwen", "pii-check", &values), "8");
        assert!(diagnostics.is_empty());

        for raw in ["abc", "nan", "-1", "0", "inf"] {
            let (values, diagnostics) =
                resolve_pair("qwen", "pii-check", &[("PII_CHECKER_TIMEOUT", raw)]);
            assert_eq!(timeout("qwen", "pii-check", &values), "5", "raw={raw}");
            assert_eq!(
                diagnostics,
                vec!["PII_CHECKER_TIMEOUT has an invalid value; using '5'"],
                "raw={raw}"
            );
        }
    }

    #[test]
    fn hooks_without_a_timeout_variable_report_their_runtime_constant() {
        for (agent, capability, expected) in [
            ("cosh", "code-scan", "10"),
            ("hermes", "prompt-scan", "15"),
            ("openclaw", "skill-ledger", "5"),
            ("qwen", "skill-ledger", "5"),
        ] {
            let (values, _) = resolve_pair(agent, capability, &[]);
            assert_eq!(timeout(agent, capability, &values), expected);
        }
    }

    #[test]
    fn l2_backend_is_reported_without_a_default_or_support_check() {
        let (values, diagnostics) = resolve_pair(
            "qoder",
            "prompt-scan",
            &[("PROMPT_SCANNER_L2_MODEL", " Unknown-Backend ")],
        );
        let value =
            find(&values, "PROMPT_SCANNER_L2_MODEL").expect("prompt-scan carries the L2 name");
        assert_eq!(value.effective, EnvValue::text("Unknown-Backend"));
        assert_eq!(value.default, EnvValue::text(""));
        assert!(diagnostics.is_empty());

        let (values, _) =
            resolve_pair("qoder", "prompt-scan", &[("PROMPT_SCANNER_L2_MODEL", "  ")]);
        let value =
            find(&values, "PROMPT_SCANNER_L2_MODEL").expect("prompt-scan carries the L2 name");
        assert_eq!(value.effective, EnvValue::text(""));
    }

    #[test]
    fn reported_values_are_escaped_and_length_capped() {
        assert_eq!(safe_value("model\u{1}\u{7f}"), "model\\x01\\x7f");
        assert_eq!(safe_value("a\u{2028}b"), "a\\u2028b");
        // Symbols outside the BMP are printable and must survive untouched.
        assert_eq!(safe_value("a\u{1d11e}b"), "a\u{1d11e}b");
        let long = "m".repeat(120);
        let capped = safe_value(&long);
        assert_eq!(capped.chars().count(), VALUE_LIMIT);
        assert!(capped.ends_with('…'));
    }

    /// Characters that rewrite how a line reads must not survive to a terminal.
    ///
    /// These are neither control nor whitespace, so they were reported verbatim
    /// until the printable test switched to general categories. A bidirectional
    /// override reverses the text after it, and a zero-width character makes two
    /// different names render identically — in the one output an operator uses to
    /// audit the configuration.
    #[test]
    fn bidi_and_zero_width_characters_are_escaped() {
        assert_eq!(
            safe_value("gpt\u{202e}4\u{200b}-evil"),
            "gpt\\u202e4\\u200b-evil"
        );
        // Every bidirectional control, not just the override.
        for character in ['\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}'] {
            let escaped = safe_value(&character.to_string());
            assert_eq!(escaped, format!("\\u{:04x}", u32::from(character)));
        }
        for character in ['\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}'] {
            let escaped = safe_value(&character.to_string());
            assert_eq!(escaped, format!("\\u{:04x}", u32::from(character)));
        }
        // Byte-order mark and private use, which V1 escapes as well.
        assert_eq!(safe_value("a\u{feff}b"), "a\\ufeffb");
        assert_eq!(safe_value("a\u{e000}b"), "a\\ue000b");
    }

    #[test]
    fn data_home_keeps_absolute_paths_and_diagnoses_the_rest() {
        let (values, diagnostics) =
            resolve_pair("cosh", "skill-ledger", &[("XDG_DATA_HOME", "/srv/anolisa")]);
        let value = find(&values, "XDG_DATA_HOME").expect("cosh ledger carries the data home");
        assert_eq!(value.effective, EnvValue::text("/srv/anolisa"));
        assert!(diagnostics.is_empty());

        for raw in ["relative/share", "/srv/../anolisa", "/srv/./anolisa"] {
            let (values, diagnostics) =
                resolve_pair("cosh", "skill-ledger", &[("XDG_DATA_HOME", raw)]);
            let value = find(&values, "XDG_DATA_HOME").expect("cosh ledger carries the data home");
            assert_eq!(
                value.effective,
                EnvValue::text("~/.local/share"),
                "raw={raw}"
            );
            assert_eq!(
                diagnostics,
                vec!["XDG_DATA_HOME has an invalid value; using '~/.local/share'"],
                "raw={raw}"
            );
        }

        // An unset-like empty value is the documented default, not an error.
        let (values, diagnostics) = resolve_pair("cosh", "skill-ledger", &[("XDG_DATA_HOME", "")]);
        let value = find(&values, "XDG_DATA_HOME").expect("cosh ledger carries the data home");
        assert_eq!(value.effective, EnvValue::text("~/.local/share"));
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn scan_mode_is_scoped_to_prompt_scan() {
        let (values, _) = resolve_pair(
            "cosh",
            "prompt-scan",
            &[("PROMPT_SCANNER_SCAN_MODE", "strict")],
        );
        assert_eq!(scan_mode("prompt-scan", &values), "strict");
        // cosh always asks, so a deeper scan must not change the interaction.
        assert_eq!(mode(&values, "ask"), "ask");

        let (values, _) = resolve_pair("cosh", "code-scan", &[]);
        assert_eq!(scan_mode("code-scan", &values), "-");
    }
}
