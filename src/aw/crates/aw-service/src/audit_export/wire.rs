//! Strict export requests and replay cursors, independent of service handles.

use aw_core::journal::reader::Position;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub api_version: String,
    pub operation: Operation,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Operation {
    Enumerate {
        cursor: Option<InventoryCursor>,
        limit: usize,
    },
    Read {
        chain_id: String,
        cursor: Option<ChainCursor>,
        limit: usize,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InventoryCursor {
    pub source_id: String,
    pub inventory_digest: String,
    pub after: String,
    pub through: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ChainCursor {
    pub source_id: String,
    pub chain_id: String,
    pub consumed: Anchor,
    pub observed_tip: Anchor,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Anchor {
    pub sequence: u64,
    pub digest: String,
}

impl From<&Anchor> for Position {
    fn from(value: &Anchor) -> Self {
        Self {
            sequence: value.sequence,
            digest: value.digest.clone(),
        }
    }
}
impl From<&Position> for Anchor {
    fn from(value: &Position) -> Self {
        Self {
            sequence: value.sequence,
            digest: value.digest.clone(),
        }
    }
}
