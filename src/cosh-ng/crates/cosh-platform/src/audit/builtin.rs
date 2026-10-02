//! Built-in audit policy presets, embedded as TOML at compile time.
//!
//! See `docs/audit-design.md` §7. Three presets are shipped:
//!   - `permissive` — sandbox / CI; allows almost everything.
//!   - `balanced`   — factory default; mirrors the historical TUI shell-tool
//!     classification with a `RequireApproval` middle state.
//!   - `strict`     — production / untrusted Agent; explicit allow-list.

use super::policy::LoadedPolicy;

const PERMISSIVE_TOML: &str = include_str!("builtin_toml/permissive.toml");
const BALANCED_TOML: &str = include_str!("builtin_toml/balanced.toml");
const STRICT_TOML: &str = include_str!("builtin_toml/strict.toml");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinPreset {
    Permissive,
    Balanced,
    Strict,
}

impl BuiltinPreset {
    pub fn name(self) -> &'static str {
        match self {
            Self::Permissive => "permissive",
            Self::Balanced => "balanced",
            Self::Strict => "strict",
        }
    }

    pub fn parse_from_token(s: &str) -> Option<Self> {
        match s {
            "permissive" => Some(Self::Permissive),
            "balanced" => Some(Self::Balanced),
            "strict" => Some(Self::Strict),
            _ => None,
        }
    }

    pub fn toml_source(self) -> &'static str {
        match self {
            Self::Permissive => PERMISSIVE_TOML,
            Self::Balanced => BALANCED_TOML,
            Self::Strict => STRICT_TOML,
        }
    }
}

pub fn load(preset: BuiltinPreset) -> LoadedPolicy {
    LoadedPolicy::from_builtin(preset, preset.toml_source())
}

pub fn permissive() -> LoadedPolicy {
    load(BuiltinPreset::Permissive)
}

pub fn balanced() -> LoadedPolicy {
    load(BuiltinPreset::Balanced)
}

pub fn strict() -> LoadedPolicy {
    load(BuiltinPreset::Strict)
}

pub const ALL: [BuiltinPreset; 3] = [
    BuiltinPreset::Permissive,
    BuiltinPreset::Balanced,
    BuiltinPreset::Strict,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_presets_parse_and_validate() {
        // from_builtin panics on parse/validate failure, so just calling
        // each constructor is the assertion.
        let _ = permissive();
        let _ = balanced();
        let _ = strict();
    }

    #[test]
    fn preset_names_round_trip() {
        for p in ALL {
            assert_eq!(BuiltinPreset::parse_from_token(p.name()), Some(p));
        }
        assert_eq!(BuiltinPreset::parse_from_token("nope"), None);
    }
}
