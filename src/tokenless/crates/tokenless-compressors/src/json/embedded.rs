//! Routing of content embedded in JSON string values to its own domain.
//!
//! The JSON tree stays the outer representation. A string value that
//! confidently carries another content domain is rendered by that domain's
//! compressor and replaced, still as a JSON string in the same slot, by the
//! rendered view behind a recovery instruction for the exact original value.
//! Field names, types, numbers, siblings and record associations are
//! untouched, so the document keeps its shape. Field names are never
//! consulted: only the value decides its kind.
//!
//! Each kind reuses an existing compressor and adds no separate pipeline;
//! HTML fragments are the first. A replacement exists only when its Stash
//! write succeeded, so every rewrite is retrievable, and its savings are
//! measured with the instruction included before anything is written.

use std::collections::HashMap;

use tokenless_ccr::{RecoveryMethod, StashStore, StashWrite, recovery_instruction};
use tokenless_protocol::estimate_tokens;

use super::JsonCompressionContext;
use super::exact::{self, ExactValue};
use crate::HtmlExtractor;

/// Shortest value considered. Below this the view header and recovery
/// instruction outweigh any markup a view could remove.
const MIN_CHARS: usize = 256;
/// Longest value rendered.
const MAX_VALUE_BYTES: usize = 256 * 1024;
/// Bytes rendered per document, across every candidate that uses the router.
const MAX_TOTAL_BYTES: usize = 2 * 1024 * 1024;
/// Render attempts per document. Values rejected by a cheap sniff do not
/// count; parsed values do, whether or not they are rewritten.
const MAX_RENDERS: usize = 128;
/// Deepest JSON nesting at which values are routed.
const MAX_DEPTH: usize = 32;
/// Estimated tokens one rewrite must save in its JSON-escaped form,
/// header and recovery instruction included.
const MIN_TOKEN_SAVINGS: usize = 16;
/// Stand-in Stash key of the real length, used to measure a replacement
/// before its value is written.
const PROBE_KEY: &str = "000000000000000000000000";

/// Content domains recognized inside string values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EmbeddedKind {
    /// An HTML fragment or document.
    Html,
}

/// One replacement for an embedded value.
#[derive(Debug, Clone)]
pub(super) struct Rewrite {
    pub(super) kind: EmbeddedKind,
    pub(super) value: String,
}

/// Per-document router shared by every candidate of one compression call.
///
/// A value seen again (a repeated field, or the same field in a later
/// candidate) reuses its first outcome, so it is rendered and stashed at
/// most once.
pub(super) struct EmbeddedRouter<'a> {
    stash: &'a dyn StashStore,
    recovery: &'a RecoveryMethod,
    remaining_bytes: usize,
    remaining_renders: usize,
    outcomes: HashMap<String, Option<Rewrite>>,
}

impl<'a> EmbeddedRouter<'a> {
    /// Builds a router when some kind is enabled and a rewrite could be
    /// recovered: a Stash is attached and the host declares a recovery path.
    pub(super) fn new(context: &JsonCompressionContext<'a>) -> Option<Self> {
        if !context.embedded_html || !context.recovery.is_available() {
            return None;
        }
        Some(Self {
            stash: context.stash?,
            recovery: context.recovery,
            remaining_bytes: MAX_TOTAL_BYTES,
            remaining_renders: MAX_RENDERS,
            outcomes: HashMap::new(),
        })
    }

    /// Returns the replacement for one string value at `depth`, or `None` to
    /// keep it. A new Stash write is appended to `writes`; a failed one is
    /// counted in `errors` and keeps the value.
    pub(super) fn route(
        &mut self,
        value: &str,
        depth: usize,
        writes: &mut Vec<StashWrite>,
        errors: &mut usize,
    ) -> Option<Rewrite> {
        // Byte length bounds the character count from above, so most short
        // values stop here; the sniff then looks only at the value's ends,
        // before anything hashes or scans the whole value.
        if depth > MAX_DEPTH || value.len() < MIN_CHARS || value.len() > MAX_VALUE_BYTES {
            return None;
        }
        let kind = classify(value)?;
        if let Some(outcome) = self.outcomes.get(value) {
            return outcome.clone();
        }
        if value.chars().count() < MIN_CHARS
            || self.remaining_renders == 0
            || self.remaining_bytes < value.len()
        {
            return None;
        }
        self.remaining_renders -= 1;
        self.remaining_bytes -= value.len();
        // A failed write is remembered like any other outcome: the value
        // stays unchanged in every candidate and the failure counts once.
        let outcome = self.rewrite(kind, value, writes, errors);
        self.outcomes.insert(value.to_owned(), outcome.clone());
        outcome
    }

    fn rewrite(
        &self,
        kind: EmbeddedKind,
        value: &str,
        writes: &mut Vec<StashWrite>,
        errors: &mut usize,
    ) -> Option<Rewrite> {
        let view = match kind {
            EmbeddedKind::Html => HtmlExtractor.render_fragment(value)?,
        };
        let probe = view.output(&recovery_instruction(PROBE_KEY, self.recovery));
        if !saves_enough(value, &probe) {
            return None;
        }
        let write = match self.stash.stash(value) {
            Ok(write) => write,
            Err(_) => {
                *errors += 1;
                return None;
            }
        };
        let rewritten = view.output(&recovery_instruction(&write.key, self.recovery));
        writes.push(write);
        Some(Rewrite {
            kind,
            value: rewritten,
        })
    }
}

/// Cheap recognition before any parsing; the domain renderer still decides.
fn classify(value: &str) -> Option<EmbeddedKind> {
    HtmlExtractor
        .sniff_fragment(value)
        .then_some(EmbeddedKind::Html)
}

/// Whether `replacement` saves at least [`MIN_TOKEN_SAVINGS`] estimated
/// tokens and some characters over `original`, both measured as they are
/// written inside a JSON document, escapes included.
fn saves_enough(original: &str, replacement: &str) -> bool {
    let original = exact::write_to_string(&ExactValue::String(original.to_owned()));
    let replacement = exact::write_to_string(&ExactValue::String(replacement.to_owned()));
    replacement.chars().count() < original.chars().count()
        && estimate_tokens(&replacement) + MIN_TOKEN_SAVINGS <= estimate_tokens(&original)
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("../tests/json_embedded_tests.rs");
}
