//! Immutable Canonical Policy IR profile metadata.

use serde::{Deserialize, Serialize};

use crate::identifiers::ProfileId;

/// IR schema version implemented by this crate.
pub const IR_SCHEMA_VERSION_V1: u16 = 1;

/// Immutable first-phase profile identifier.
pub const PROFILE_V1ALPHA1_DEMO1: &str = "agentseccore-canonical-ir/v1alpha1-demo1";

/// Maximum resource sets accepted by the first-phase profile.
pub const MAX_RESOURCE_SETS: usize = 256;

/// Maximum rules accepted by the first-phase profile.
pub const MAX_RULES: usize = 256;

/// Maximum atoms in one V1 `All` expression.
pub const MAX_ATOMS_PER_RULE: usize = 8;

/// Semantic Atom category advertised by a profile or adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AtomKind {
    /// A normalized operation on one resource domain.
    ResourceOperation,
    /// Information flow admitted by the selected profile.
    InformationFlow,
}

/// Expression shape advertised by a profile or adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpressionKind {
    /// One semantic atom.
    Atom,
    /// Conjunction evaluated against one decision event.
    All,
    /// Disjunction. Not accepted by the first-phase profile.
    Any,
    /// Negation. Not accepted by the first-phase profile.
    Not,
}

/// Immutable limits and syntax admitted by one IR profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IrProfile {
    /// Immutable profile identity.
    pub profile_id: ProfileId,
    /// Allowed semantic Atom categories.
    pub allowed_atoms: Vec<AtomKind>,
    /// Allowed expression shapes.
    pub allowed_expressions: Vec<ExpressionKind>,
    /// Maximum rules in one payload.
    pub max_rules: u16,
    /// Maximum atoms in one rule.
    pub max_atoms_per_rule: u16,
}

impl IrProfile {
    /// Returns the first-phase immutable profile.
    ///
    /// # Panics
    /// Panics only if the compile-time profile identifier violates the shared
    /// identifier alphabet.
    pub fn v1alpha1_demo1() -> Self {
        Self {
            profile_id: ProfileId::new(PROFILE_V1ALPHA1_DEMO1)
                .expect("compile-time profile id must be valid"),
            allowed_atoms: vec![AtomKind::ResourceOperation, AtomKind::InformationFlow],
            allowed_expressions: vec![ExpressionKind::Atom, ExpressionKind::All],
            max_rules: u16::try_from(MAX_RULES).expect("profile limit fits u16"),
            max_atoms_per_rule: u16::try_from(MAX_ATOMS_PER_RULE).expect("profile limit fits u16"),
        }
    }
}
