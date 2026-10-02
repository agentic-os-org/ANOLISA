use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::ops::Deref;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingSeverity {
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookFinding {
    pub hook_id: String,
    pub severity: FindingSeverity,
    pub title: String,
    pub description: String,
    pub suggestion: String,
    pub skill: Option<String>,
    pub cli_hint: Option<String>,
    #[serde(default)]
    pub context_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookProvenance {
    Builtin {
        producer_registration_ids: BTreeSet<String>,
    },
    External {
        registration_key: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricsConfidence {
    Low,
    High,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryPressureFacts {
    pub confidence: MetricsConfidence,
    pub available_ratio: f64,
    pub swap_ratio: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProcessMemoryFact {
    pub pid: String,
    pub command_basename: String,
    pub mem_pct: f64,
    pub rss_kib: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HighMemoryProcessFacts {
    pub confidence: MetricsConfidence,
    pub processes: Vec<ProcessMemoryFact>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BuiltinFindingFacts {
    MemoryPressure(MemoryPressureFacts),
    HighMemoryProcesses(HighMemoryProcessFacts),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BuiltinFactRecord {
    pub(crate) producer_registration_id: String,
    pub(crate) facts: BuiltinFindingFacts,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EvaluatedHookFinding {
    pub(crate) provenance: HookProvenance,
    pub(crate) finding: HookFinding,
    pub(crate) builtin_facts: Option<BuiltinFindingFacts>,
}

impl EvaluatedHookFinding {
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn builtin(registration_id: impl Into<String>, finding: HookFinding) -> Self {
        Self {
            provenance: HookProvenance::Builtin {
                producer_registration_ids: BTreeSet::from([registration_id.into()]),
            },
            finding,
            builtin_facts: None,
        }
    }

    pub(crate) fn builtin_with_facts(
        registration_id: impl Into<String>,
        finding: HookFinding,
        builtin_facts: Option<BuiltinFindingFacts>,
    ) -> Self {
        Self {
            provenance: HookProvenance::Builtin {
                producer_registration_ids: BTreeSet::from([registration_id.into()]),
            },
            finding,
            builtin_facts,
        }
    }

    pub(crate) fn external(registration_key: impl Into<String>, finding: HookFinding) -> Self {
        Self {
            provenance: HookProvenance::External {
                registration_key: registration_key.into(),
            },
            finding,
            builtin_facts: None,
        }
    }

    pub fn provenance(&self) -> &HookProvenance {
        &self.provenance
    }

    pub fn finding(&self) -> &HookFinding {
        &self.finding
    }
}

impl Deref for EvaluatedHookFinding {
    type Target = HookFinding;

    fn deref(&self) -> &Self::Target {
        &self.finding
    }
}

#[cfg(test)]
impl From<HookFinding> for EvaluatedHookFinding {
    fn from(finding: HookFinding) -> Self {
        Self::external("test-unregistered", finding)
    }
}
