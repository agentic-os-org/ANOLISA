//! Static agent, capability and environment-variable manifest for the view.
//!
//! Item G2 in `docs/design/V2_CAPABILITY_VIEW_MIGRATION_zh.md`: this table is
//! the Rust fork of the V1 `agent_sec_cli.capabilities.view` manifest, which the
//! contract-first rewrite implies rather than a defect. Both copies must change
//! together while the two generations ship side by side, otherwise the two CLIs
//! report different configuration for the same environment.

/// Reported agents in the order used by filter errors and default listings.
pub(crate) const AGENTS: [&str; 6] = ["qoder", "qwen", "codex", "cosh", "openclaw", "hermes"];

/// Canonical capability names; hook-level aliases such as `scan-code` are rejected.
pub(crate) const CANONICAL_CAPABILITIES: [&str; 5] = [
    "code-scan",
    "prompt-scan",
    "pii-check",
    "skill-ledger",
    "observability",
];

const HOOK_POLICIES: &[&str] = &["observe", "warn", "ask", "block"];
const CODE_MODES_INTERACTIVE: &[&str] = &["observe", "ask", "block"];
const CODE_MODES_BLOCK_ONLY: &[&str] = &["observe", "block"];
const HERMES_NATIVE_MODES: &[&str] = &["observe", "block"];
const PROMPT_MODES: &[&str] = &["observe", "deny"];
const SCAN_MODES: &[&str] = &["fast", "standard", "strict"];
const ASK_ONLY: &[&str] = &["ask"];

/// How one environment variable is parsed and which value it falls back to.
#[derive(Debug, Clone, Copy)]
pub(crate) enum EnvKind {
    /// Accepts `true`/`false` only, matching the hook enable switches.
    StrictBool(bool),
    /// Accepts `1/true/yes/on` and `0/false/no/off`, matching legacy switches.
    BroadBool(bool),
    /// Keyword with an allowlist; `aliased` enables the hook-policy aliases.
    Keyword {
        default: &'static str,
        allowed: &'static [&'static str],
        aliased: bool,
    },
    /// Integer seconds; `require_positive` diagnoses instead of reporting `<= 0`.
    IntTimeout {
        default: &'static str,
        max: Option<i64>,
        require_positive: bool,
    },
    /// Fractional seconds, clamped to `max` when present.
    FloatTimeout {
        default: &'static str,
        max: Option<f64>,
    },
    /// Opaque prompt-scanner L2 backend name; see gap G1.
    Identifier,
    /// ANOLISA data root, validated syntactically without touching the filesystem.
    DataHome { default: &'static str },
}

/// One environment variable contributing to a capability record.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EnvSpec {
    pub(crate) name: &'static str,
    pub(crate) kind: EnvKind,
}

/// Environment contract of one agent/capability pair.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CapabilitySpec {
    pub(crate) env: &'static [EnvSpec],
    /// Interaction mode reported when the pair exposes no `*_MODE` variable.
    pub(crate) default_mode: &'static str,
}

const fn hook_enabled(name: &'static str) -> EnvSpec {
    EnvSpec {
        name,
        kind: EnvKind::StrictBool(true),
    }
}

const fn broad_bool(name: &'static str, default: bool) -> EnvSpec {
    EnvSpec {
        name,
        kind: EnvKind::BroadBool(default),
    }
}

const fn mode(
    name: &'static str,
    default: &'static str,
    allowed: &'static [&'static str],
) -> EnvSpec {
    EnvSpec {
        name,
        kind: EnvKind::Keyword {
            default,
            allowed,
            aliased: true,
        },
    }
}

const fn plain_mode(
    name: &'static str,
    default: &'static str,
    allowed: &'static [&'static str],
) -> EnvSpec {
    EnvSpec {
        name,
        kind: EnvKind::Keyword {
            default,
            allowed,
            aliased: false,
        },
    }
}

const fn int_timeout(name: &'static str, default: &'static str) -> EnvSpec {
    EnvSpec {
        name,
        kind: EnvKind::IntTimeout {
            default,
            max: None,
            require_positive: false,
        },
    }
}

const fn float_timeout(name: &'static str, default: &'static str, max: Option<f64>) -> EnvSpec {
    EnvSpec {
        name,
        kind: EnvKind::FloatTimeout { default, max },
    }
}

const OBSERVABILITY_ENV: &[EnvSpec] = &[
    hook_enabled("OBSERVABILITY_HOOK_ENABLED"),
    EnvSpec {
        name: "OBSERVABILITY_TIMEOUT",
        kind: EnvKind::IntTimeout {
            default: "5",
            max: Some(5),
            require_positive: true,
        },
    },
];

const L2_MODEL: EnvSpec = EnvSpec {
    name: "PROMPT_SCANNER_L2_MODEL",
    kind: EnvKind::Identifier,
};

const PROMPT_SCAN_MODE: EnvSpec = plain_mode("PROMPT_SCANNER_SCAN_MODE", "standard", SCAN_MODES);

const QODER_CODE: &[EnvSpec] = &[
    hook_enabled("CODE_SCANNER_HOOK_ENABLED"),
    mode("CODE_SCANNER_MODE", "observe", CODE_MODES_INTERACTIVE),
    int_timeout("CODE_SCANNER_TIMEOUT", "10"),
];
const QODER_PROMPT: &[EnvSpec] = &[
    hook_enabled("PROMPT_SCANNER_HOOK_ENABLED"),
    plain_mode("PROMPT_SCANNER_MODE", "observe", PROMPT_MODES),
    PROMPT_SCAN_MODE,
    L2_MODEL,
    int_timeout("PROMPT_SCANNER_TIMEOUT", "10"),
];
const QODER_PII: &[EnvSpec] = &[
    hook_enabled("PII_CHECKER_HOOK_ENABLED"),
    mode("PII_CHECKER_MODE", "observe", HOOK_POLICIES),
    broad_bool("PII_CHECKER_INCLUDE_LOW_CONFIDENCE", false),
    int_timeout("PII_CHECKER_TIMEOUT", "5"),
];
const QODER_LEDGER: &[EnvSpec] = &[
    hook_enabled("SKILL_LEDGER_HOOK_ENABLED"),
    mode("SKILL_LEDGER_MODE", "ask", HOOK_POLICIES),
    float_timeout("SKILL_LEDGER_TIMEOUT", "5", None),
];

const QWEN_PII: &[EnvSpec] = &[
    hook_enabled("PII_CHECKER_HOOK_ENABLED"),
    broad_bool("PII_CHECKER_ENABLED", true),
    mode("PII_CHECKER_MODE", "observe", HOOK_POLICIES),
    broad_bool("PII_CHECKER_INCLUDE_LOW_CONFIDENCE", false),
    float_timeout("PII_CHECKER_TIMEOUT", "5", Some(8.0)),
];
const QWEN_LEDGER: &[EnvSpec] = &[
    hook_enabled("SKILL_LEDGER_HOOK_ENABLED"),
    mode("SKILL_LEDGER_MODE", "ask", HOOK_POLICIES),
];

const CODEX_CODE: &[EnvSpec] = &[
    hook_enabled("CODE_SCANNER_HOOK_ENABLED"),
    mode("CODE_SCANNER_MODE", "observe", CODE_MODES_BLOCK_ONLY),
    int_timeout("CODE_SCANNER_TIMEOUT", "10"),
];
const CODEX_PII: &[EnvSpec] = &[
    hook_enabled("PII_CHECKER_HOOK_ENABLED"),
    mode("PII_CHECKER_MODE", "observe", HOOK_POLICIES),
    int_timeout("PII_CHECKER_TIMEOUT", "5"),
];
const CODEX_LEDGER: &[EnvSpec] = &[
    hook_enabled("SKILL_LEDGER_HOOK_ENABLED"),
    mode("SKILL_LEDGER_MODE", "ask", HOOK_POLICIES),
    int_timeout("SKILL_LEDGER_TIMEOUT", "5"),
];

const COSH_CODE: &[EnvSpec] = &[
    hook_enabled("CODE_SCANNER_HOOK_ENABLED"),
    mode("CODE_SCANNER_MODE", "ask", ASK_ONLY),
];
const COSH_PROMPT: &[EnvSpec] = &[
    hook_enabled("PROMPT_SCANNER_HOOK_ENABLED"),
    PROMPT_SCAN_MODE,
    L2_MODEL,
];
const COSH_PII: &[EnvSpec] = &[
    hook_enabled("PII_CHECKER_HOOK_ENABLED"),
    mode("PII_CHECKER_MODE", "observe", HOOK_POLICIES),
];
const COSH_LEDGER: &[EnvSpec] = &[
    hook_enabled("SKILL_LEDGER_HOOK_ENABLED"),
    mode("SKILL_LEDGER_MODE", "ask", HOOK_POLICIES),
    EnvSpec {
        name: "XDG_DATA_HOME",
        kind: EnvKind::DataHome {
            default: "~/.local/share",
        },
    },
];

const OPENCLAW_CODE: &[EnvSpec] = &[
    hook_enabled("CODE_SCANNER_HOOK_ENABLED"),
    mode("CODE_SCANNER_MODE", "observe", CODE_MODES_INTERACTIVE),
];
const HERMES_CODE: &[EnvSpec] = &[
    hook_enabled("CODE_SCANNER_HOOK_ENABLED"),
    mode("CODE_SCANNER_MODE", "observe", CODE_MODES_BLOCK_ONLY),
];
const HERMES_PII: &[EnvSpec] = &[
    hook_enabled("PII_CHECKER_HOOK_ENABLED"),
    mode("PII_CHECKER_MODE", "observe", HERMES_NATIVE_MODES),
];
const HERMES_LEDGER: &[EnvSpec] = &[
    hook_enabled("SKILL_LEDGER_HOOK_ENABLED"),
    mode("SKILL_LEDGER_MODE", "observe", HERMES_NATIVE_MODES),
];

/// Returns the environment contract of one agent/capability pair.
///
/// # Panics
/// Panics when either name is not in `AGENTS` / `CANONICAL_CAPABILITIES`;
/// callers normalize both through the filter parser first.
pub(crate) fn spec(agent: &str, capability: &str) -> CapabilitySpec {
    let (env, default_mode) = match (agent, capability) {
        ("qoder" | "qwen", "code-scan") => (QODER_CODE, "observe"),
        ("qoder" | "qwen" | "codex", "prompt-scan") => (QODER_PROMPT, "observe"),
        ("qoder", "pii-check") => (QODER_PII, "observe"),
        ("qoder", "skill-ledger") => (QODER_LEDGER, "ask"),
        ("qwen", "pii-check") => (QWEN_PII, "observe"),
        ("qwen" | "openclaw", "skill-ledger") => (QWEN_LEDGER, "ask"),
        ("codex", "code-scan") => (CODEX_CODE, "observe"),
        ("codex", "pii-check") => (CODEX_PII, "observe"),
        ("codex", "skill-ledger") => (CODEX_LEDGER, "ask"),
        ("cosh", "code-scan") => (COSH_CODE, "ask"),
        // The cosh prompt hook always asks the operator, so its reported mode
        // comes from the manifest rather than a PROMPT_SCANNER_MODE variable.
        ("cosh", "prompt-scan") => (COSH_PROMPT, "ask"),
        ("cosh" | "openclaw", "pii-check") => (COSH_PII, "observe"),
        ("cosh", "skill-ledger") => (COSH_LEDGER, "ask"),
        ("openclaw", "code-scan") => (OPENCLAW_CODE, "observe"),
        ("openclaw" | "hermes", "prompt-scan") => (COSH_PROMPT, "observe"),
        ("hermes", "code-scan") => (HERMES_CODE, "observe"),
        ("hermes", "pii-check") => (HERMES_PII, "observe"),
        // Hermes delivers advisories natively, so its ledger hook reports
        // observe instead of the ask default used by subprocess hooks.
        ("hermes", "skill-ledger") => (HERMES_LEDGER, "observe"),
        (_, "observability") if AGENTS.contains(&agent) => (OBSERVABILITY_ENV, "observe"),
        _ => unreachable!("unknown agent/capability pair: {agent}/{capability}"),
    };
    CapabilitySpec { env, default_mode }
}

/// Timeout reported for pairs whose hook has no timeout variable of its own.
///
/// The values are the hard-coded runtime timeouts of the corresponding native
/// or in-process hook, so the view never shows `-` for a hook that does time
/// out.
pub(crate) fn static_default_timeout(agent: &str, capability: &str) -> Option<&'static str> {
    let timeout = match (agent, capability) {
        ("qwen" | "cosh" | "openclaw" | "hermes", "skill-ledger") => "5",
        ("cosh" | "openclaw", "code-scan" | "pii-check" | "prompt-scan")
        | ("hermes", "code-scan" | "pii-check") => "10",
        ("hermes", "prompt-scan") => "15",
        _ => return None,
    };
    Some(timeout)
}

/// Every variable name the view can report, deduplicated.
///
/// Callers use it to read only the relevant part of the process environment,
/// which keeps an unrelated variable from influencing the view at all.
pub(crate) fn env_names() -> Vec<&'static str> {
    let mut names = Vec::new();
    for agent in AGENTS {
        for capability in CANONICAL_CAPABILITIES {
            for entry in spec(agent, capability).env {
                if !names.contains(&entry.name) {
                    names.push(entry.name);
                }
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_agent_capability_pair_has_a_spec() {
        for agent in AGENTS {
            for capability in CANONICAL_CAPABILITIES {
                let spec = spec(agent, capability);
                assert!(!spec.env.is_empty(), "{agent}/{capability} has no env");
                assert!(
                    spec.env
                        .iter()
                        .any(|entry| entry.name.ends_with("_HOOK_ENABLED")),
                    "{agent}/{capability} has no enable switch"
                );
            }
        }
    }

    #[test]
    fn static_defaults_cover_every_pair_without_a_timeout_variable() {
        for agent in AGENTS {
            for capability in CANONICAL_CAPABILITIES {
                let has_variable = spec(agent, capability)
                    .env
                    .iter()
                    .any(|entry| entry.name.ends_with("_TIMEOUT"));
                assert_eq!(
                    has_variable,
                    static_default_timeout(agent, capability).is_none(),
                    "{agent}/{capability} timeout source is ambiguous"
                );
            }
        }
    }

    #[test]
    fn prompt_scan_is_the_only_capability_carrying_the_l2_backend() {
        for agent in AGENTS {
            for capability in CANONICAL_CAPABILITIES {
                let carries = spec(agent, capability)
                    .env
                    .iter()
                    .any(|entry| entry.name == "PROMPT_SCANNER_L2_MODEL");
                assert_eq!(carries, capability == "prompt-scan", "{agent}/{capability}");
            }
        }
    }
}
