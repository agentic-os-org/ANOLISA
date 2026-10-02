//! Configuration presets for the prompt scanner.

use std::str::FromStr;

use crate::error::ScannerError;
use crate::models::multi_turn_intent::DEFAULT_HARMFUL_THRESHOLD;
use crate::models::qwen3_guard::MODEL_QWEN3_GUARD;

/// Predefined detection mode presets.
///
/// - `Fast`:      L1 only.  Real-time chat scenarios.
/// - `Standard`:  L1 + L2.  Recommended for most production use.
/// - `Strict`:    Reserved; currently identical to `Standard`.
/// - `MultiTurn`: L4 only.  Judges a full conversation triple (history,
///   current query, assistant response) and is decoupled from L1/L2
///   because it consumes a richer input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanMode {
    Fast,
    Standard,
    Strict,
    MultiTurn,
}

impl ScanMode {
    /// Stable wire value used in CLI arguments and JSON output.
    pub fn as_str(&self) -> &'static str {
        match self {
            ScanMode::Fast => "fast",
            ScanMode::Standard => "standard",
            ScanMode::Strict => "strict",
            ScanMode::MultiTurn => "multi_turn",
        }
    }
}

impl FromStr for ScanMode {
    type Err = ScannerError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "fast" => Ok(ScanMode::Fast),
            "standard" => Ok(ScanMode::Standard),
            "strict" => Ok(ScanMode::Strict),
            "multi_turn" => Ok(ScanMode::MultiTurn),
            other => Err(ScannerError::Config(format!("Unknown scan mode: {other}"))),
        }
    }
}

/// Full configuration for a [`PromptScanner`](crate::scanner::PromptScanner) instance.
#[derive(Debug, Clone)]
pub struct ScanConfig {
    /// Enabled detector names (ordered).
    pub layers: Vec<String>,
    /// Stop on first positive detection.
    pub fast_fail: bool,
    /// Attempt to decode obfuscated encodings (Base64, ROT13, etc.).
    pub detect_encoding: bool,
    /// L2 model identifier.
    pub model_name: String,
    /// L4 `p_harmful` threshold above which the verdict is a block.
    pub multi_turn_threshold: f64,
}

impl Default for ScanConfig {
    fn default() -> Self {
        ScanConfig {
            layers: vec!["rule_engine".to_string(), "ml_classifier".to_string()],
            fast_fail: true,
            detect_encoding: true,
            model_name: MODEL_QWEN3_GUARD.to_string(),
            multi_turn_threshold: DEFAULT_HARMFUL_THRESHOLD,
        }
    }
}

impl ScanConfig {
    /// Preset configuration for the given mode.
    pub fn preset(mode: ScanMode) -> Self {
        match mode {
            ScanMode::Fast => ScanConfig {
                layers: vec!["rule_engine".to_string()],
                fast_fail: true,
                ..ScanConfig::default()
            },
            ScanMode::Standard | ScanMode::Strict => ScanConfig {
                layers: vec!["rule_engine".to_string(), "ml_classifier".to_string()],
                fast_fail: false,
                ..ScanConfig::default()
            },
            // L4 runs alone: it consumes a conversation triple and
            // delegates to an external service.  When that service is
            // unreachable, construction fails so the caller knows the
            // multi-turn check could not be performed.
            ScanMode::MultiTurn => ScanConfig {
                layers: vec!["multi_turn_intent".to_string()],
                fast_fail: false,
                ..ScanConfig::default()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_round_trip() {
        for mode in [
            ScanMode::Fast,
            ScanMode::Standard,
            ScanMode::Strict,
            ScanMode::MultiTurn,
        ] {
            assert_eq!(mode.as_str().parse::<ScanMode>().unwrap(), mode);
        }
        assert!("banana".parse::<ScanMode>().is_err());
    }

    #[test]
    fn presets_declare_the_expected_layers() {
        let fast = ScanConfig::preset(ScanMode::Fast);
        assert_eq!(fast.layers, vec!["rule_engine"]);
        assert!(fast.fast_fail);

        let standard = ScanConfig::preset(ScanMode::Standard);
        assert_eq!(standard.layers, vec!["rule_engine", "ml_classifier"]);
        assert!(!standard.fast_fail);

        let strict = ScanConfig::preset(ScanMode::Strict);
        assert_eq!(strict.layers, standard.layers);

        let multi_turn = ScanConfig::preset(ScanMode::MultiTurn);
        assert_eq!(multi_turn.layers, vec!["multi_turn_intent"]);
        assert!(!multi_turn.fast_fail);
    }

    #[test]
    fn defaults_target_qwen3guard_and_the_documented_threshold() {
        let config = ScanConfig::default();
        assert_eq!(config.model_name, MODEL_QWEN3_GUARD);
        assert_eq!(config.multi_turn_threshold, 0.55);
        assert!(config.detect_encoding);
    }
}
