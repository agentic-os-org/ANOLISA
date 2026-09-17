//! Verdict derivation — map layer detection results to a final [`Verdict`].

use crate::result::{LayerResult, Verdict};

/// Layers whose detection is treated as a confirmed threat → DENY.
///
/// L1 (rule_engine) alone → WARN because regex has a higher false-positive
/// rate and its signal should be confirmed by L2 when L2 is present.
/// L4 multi_turn_intent is authoritative for its own dimension.
const CONFIRM_LAYERS: [&str; 2] = ["ml_classifier", "multi_turn_intent"];

fn is_confirm_layer(name: &str) -> bool {
    CONFIRM_LAYERS.contains(&name)
}

/// Derive a [`Verdict`] from detection results across all layers.
///
/// Decision rules (evaluated in order):
///
/// 1. Any confirm-layer (L2 ML or L4 multi_turn_intent) detected → DENY
/// 2. L1 detected AND no confirm-layer was present (FAST mode) → DENY
/// 3. L1 detected AND confirm-layer present but did not fire → WARN
/// 4. No layer detected → PASS
pub fn determine_verdict(layer_results: &[LayerResult]) -> Verdict {
    let confirmed = layer_results
        .iter()
        .any(|lr| lr.detected && is_confirm_layer(&lr.layer_name));
    if confirmed {
        return Verdict::Deny;
    }

    let any_detected = layer_results.iter().any(|lr| lr.detected);
    if any_detected {
        let confirm_layer_ran = layer_results
            .iter()
            .any(|lr| is_confirm_layer(&lr.layer_name));
        if confirm_layer_ran {
            // Confirm-layer ran but did not confirm → possible L1 false-positive.
            return Verdict::Warn;
        }
        // FAST mode: no L2, L1 is sole authority.
        return Verdict::Deny;
    }

    Verdict::Pass
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(name: &str, detected: bool) -> LayerResult {
        LayerResult {
            layer_name: name.to_string(),
            detected,
            score: Some(if detected { 0.9 } else { 0.0 }),
            details: vec![],
            latency_ms: 0.0,
        }
    }

    #[test]
    fn confirm_layer_hit_is_deny() {
        // L2 confirms regardless of L1.
        let results = vec![layer("rule_engine", false), layer("ml_classifier", true)];
        assert_eq!(determine_verdict(&results), Verdict::Deny);

        let results = vec![layer("multi_turn_intent", true)];
        assert_eq!(determine_verdict(&results), Verdict::Deny);
    }

    #[test]
    fn l1_only_hit_is_deny() {
        // FAST mode: no confirm-layer ran, L1 is sole authority.
        let results = vec![layer("rule_engine", true)];
        assert_eq!(determine_verdict(&results), Verdict::Deny);
    }

    #[test]
    fn l1_hit_unconfirmed_is_warn() {
        // Confirm-layer ran but did not fire → WARN.
        let results = vec![layer("rule_engine", true), layer("ml_classifier", false)];
        assert_eq!(determine_verdict(&results), Verdict::Warn);
    }

    #[test]
    fn nothing_detected_is_pass() {
        let results = vec![layer("rule_engine", false), layer("ml_classifier", false)];
        assert_eq!(determine_verdict(&results), Verdict::Pass);
        assert_eq!(determine_verdict(&[]), Verdict::Pass);
    }
}
