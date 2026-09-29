//! Truncation thresholds for the unified entry point (roadmap §5.4).
//!
//! Thresholds come from the common Python hooks (`hook_utils.py`), sourced
//! from the same `adapters/tokenless/common/hooks/tool_categories.json` so
//! the JSON file stays the single source of truth (OpenClaw still reads it
//! at runtime).
//! The file is embedded at compile time — the binary must work without
//! adapter files installed — which ties this crate to the workspace layout;
//! acceptable for a workspace-internal crate that is never `cargo package`d.
//!
//! A malformed edit of the JSON falls back to the hardcoded thresholds and
//! the unit tests fail so CI reports the breakage instead of degrading.

use std::sync::OnceLock;

use serde::Deserialize;
use tokenless_protocol::ContentOrigin;

static TAXONOMY_JSON: &str =
    include_str!("../../../adapters/tokenless/common/hooks/tool_categories.json");

/// Per-tool truncation limits handed to the response compressor
/// (layer 2 shell vs layer 3 API in `tool_categories.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ToolThresholds {
    pub truncate_strings_at: usize,
    pub truncate_arrays_at: usize,
    pub max_depth: usize,
}

const SHELL_THRESHOLDS: ToolThresholds = ToolThresholds {
    truncate_strings_at: 65_536,
    truncate_arrays_at: 128,
    max_depth: 8,
};

const API_THRESHOLDS: ToolThresholds = ToolThresholds {
    truncate_strings_at: 1_048_576,
    truncate_arrays_at: 65_536,
    max_depth: 32,
};

#[derive(Deserialize)]
struct RawTaxonomy {
    layer_2_shell: RawLayer,
    layer_3_api: RawLayer,
}

#[derive(Deserialize)]
struct RawLayer {
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    thresholds: Option<RawThresholds>,
}

#[derive(Deserialize)]
struct RawThresholds {
    truncate_strings_at: Option<usize>,
    truncate_arrays_at: Option<usize>,
    max_depth: Option<usize>,
}

impl RawThresholds {
    fn resolve(raw: Option<Self>, defaults: ToolThresholds) -> ToolThresholds {
        let raw = match raw {
            Some(raw) => raw,
            None => return defaults,
        };
        ToolThresholds {
            truncate_strings_at: raw
                .truncate_strings_at
                .unwrap_or(defaults.truncate_strings_at),
            truncate_arrays_at: raw
                .truncate_arrays_at
                .unwrap_or(defaults.truncate_arrays_at),
            max_depth: raw.max_depth.unwrap_or(defaults.max_depth),
        }
    }
}

struct Taxonomy {
    shell_thresholds: ToolThresholds,
    api_thresholds: ToolThresholds,
}

fn fallback_taxonomy() -> Taxonomy {
    Taxonomy {
        shell_thresholds: SHELL_THRESHOLDS,
        api_thresholds: API_THRESHOLDS,
    }
}

fn taxonomy() -> &'static Taxonomy {
    static TAXONOMY: OnceLock<Taxonomy> = OnceLock::new();
    TAXONOMY.get_or_init(
        || match serde_json::from_str::<RawTaxonomy>(TAXONOMY_JSON) {
            Ok(raw) if !raw.layer_2_shell.tools.is_empty() => Taxonomy {
                shell_thresholds: RawThresholds::resolve(
                    raw.layer_2_shell.thresholds,
                    SHELL_THRESHOLDS,
                ),
                api_thresholds: RawThresholds::resolve(raw.layer_3_api.thresholds, API_THRESHOLDS),
            },
            _ => fallback_taxonomy(),
        },
    )
}

/// Truncation thresholds: layer 2 limits for command output, including
/// local files printed by a shell command, layer 3 zero-truncation limits
/// for everything else.
///
/// The required origin decides the threshold family without tool-name
/// fallback.
pub(crate) fn thresholds_for(origin: ContentOrigin) -> ToolThresholds {
    let tax = taxonomy();
    match origin {
        ContentOrigin::CommandOutput | ContentOrigin::FileRead => tax.shell_thresholds,
        ContentOrigin::FileContent | ContentOrigin::ApiResponse => tax.api_thresholds,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Drift guard for the embedded JSON: a malformed edit must fail here,
    // not silently fall back at runtime.
    #[test]
    fn embedded_taxonomy_parses_with_expected_layers() {
        let raw: RawTaxonomy = serde_json::from_str(TAXONOMY_JSON).expect("embedded JSON parses");
        assert!(raw.layer_2_shell.tools.contains(&"Bash".to_string()));
        assert!(raw.layer_2_shell.thresholds.is_some());
        assert!(raw.layer_3_api.thresholds.is_some());
    }

    #[test]
    fn origin_selects_thresholds() {
        assert_eq!(
            thresholds_for(ContentOrigin::CommandOutput),
            SHELL_THRESHOLDS
        );
        assert_eq!(thresholds_for(ContentOrigin::ApiResponse), API_THRESHOLDS);
        assert_eq!(thresholds_for(ContentOrigin::FileContent), API_THRESHOLDS);
        assert_eq!(thresholds_for(ContentOrigin::FileRead), SHELL_THRESHOLDS);
    }

    #[test]
    fn fallback_matches_the_embedded_values() {
        // The hardcoded fallback must stay in sync with the JSON defaults,
        // so a fallback activation changes availability, never behaviour.
        let fallback = fallback_taxonomy();
        let parsed = taxonomy();
        assert_eq!(fallback.shell_thresholds, parsed.shell_thresholds);
        assert_eq!(fallback.api_thresholds, parsed.api_thresholds);
    }
}
