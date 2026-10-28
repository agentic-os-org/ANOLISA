//! Replace one unit in the closed scenario registry without adding a wire scenario.

use std::sync::Arc;

use semantic_arbiter::contract::{AnswerValue, Reason, Scenario, UnitAnswer};
use semantic_arbiter::judgment::{JudgmentUnit, JudgmentUnitRegistry, TaskScopeUnit};
use semantic_arbiter::model::{NoulProbability, NoulQuestion};

struct ReviewedPreservationUnit;

impl JudgmentUnit for ReviewedPreservationUnit {
    fn scenario(&self) -> Scenario {
        Scenario::Preservation
    }

    fn revision(&self) -> &'static str {
        "reviewed-preservation-r1"
    }

    fn question_id(&self) -> &'static str {
        "reviewed-preservation-v1"
    }

    fn question(&self) -> NoulQuestion {
        NoulQuestion::new("Does the reviewed task require preserving this resource?".into())
    }

    fn decide(
        &self,
        baseline: &UnitAnswer,
        probability: &NoulProbability,
        threshold: u32,
    ) -> UnitAnswer {
        if probability.supports_yes(threshold) {
            UnitAnswer {
                unit: baseline.unit,
                value: AnswerValue::Preserve,
                confidence_ppm: probability.yes_ppm(),
                reason: Reason::Preserved,
                evidence_refs: baseline.evidence_refs.clone(),
            }
        } else {
            UnitAnswer {
                unit: baseline.unit,
                value: AnswerValue::Unknown,
                confidence_ppm: 0,
                reason: Reason::Undetermined,
                evidence_refs: baseline.evidence_refs.clone(),
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let registry =
        JudgmentUnitRegistry::new(Arc::new(ReviewedPreservationUnit), Arc::new(TaskScopeUnit))?;
    println!(
        "{} {}",
        registry.unit(Scenario::Preservation).question_id(),
        registry.unit(Scenario::TaskScope).question_id()
    );
    Ok(())
}
