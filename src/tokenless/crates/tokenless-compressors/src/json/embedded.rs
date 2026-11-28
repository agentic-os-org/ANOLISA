//! Content-domain routing for JSON string values.
//!
//! Structured tool responses often carry rendered markup, compiler logs,
//! CSV/TSV exports, or doubly-serialized JSON envelopes inside string fields.
//! When enabled, a string leaf that confidently carries another content domain
//! is rendered by that domain's compressor and replaced, still as a JSON
//! string in the same slot, by the rendered view behind a recovery instruction
//! for the exact original value. Serialized JSON containers (`{...}` or
//! `[...]`) are scanned in place so nested string values inside them can be
//! routed recursively without turning the outer string into an object or
//! altering surrounding whitespace, key order, or exact numeric literals.
//! Field names are never consulted: only the value decides its kind.
//!
//! Each kind reuses an existing compressor and adds no separate pipeline. A
//! leaf replacement exists only when its Stash write succeeded, so every
//! rewrite is retrievable, and its savings and outer string bounds are checked
//! in memory with the real recovery instruction included before any leaf is
//! written to Stash.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokenless_ccr::{
    RecoveryMethod, StashStore, StashWrite, compute_digest, compute_key, recovery_instruction,
};
use tokenless_protocol::estimate_tokens;

use super::JsonCompressionContext;
use super::exact::{self, StringSlot};
use crate::{BuildLogCompressor, BuildLogMode, HtmlExtractor, TabularCompressor};

/// Shortest value considered. Below this the view header and recovery
/// instruction outweigh any markup or repetition a view could remove.
const MIN_CHARS: usize = 256;
/// Longest value rendered.
const MAX_VALUE_BYTES: usize = 256 * 1024;
/// Total source bytes rendered across one JSON document.
const MAX_TOTAL_BYTES: usize = 2 * 1024 * 1024;
/// Maximum number of distinct candidate values rendered in one JSON document.
const MAX_RENDERS: usize = 128;
/// Maximum number of negative (non-compressible) digests cached in one JSON document.
const MAX_NEGATIVE_CACHE_ENTRIES: usize = 1024;
/// Deepest JSON nesting at which values are routed.
const MAX_DEPTH: usize = 32;
/// Estimated tokens one leaf rewrite must save in its JSON-escaped form,
/// header and recovery instruction included.
const MIN_TOKEN_SAVINGS: usize = 16;

pub(super) const HTML_VIEW_HEADER_PREFIX: &str = "[HTML rendered as Markdown";
pub(super) const BUILD_LOG_VIEW_HEADER_PREFIX: &str = "[Build log reduced; ";
pub(super) const TABULAR_VIEW_HEADER_PREFIX: &str = "[Tabular data compacted; ";

/// Content domains recognized inside string values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EmbeddedKind {
    /// An HTML fragment or document.
    Html,
    /// Compiler, package-manager, or test-runner output.
    BuildLog,
    /// Rectangular header-first CSV or TSV data.
    Tabular,
    /// A serialized JSON object or array whose inner string values are routed
    /// in place while preserving the outer JSON string representation.
    Json,
}

/// Fixed-capacity stack list of candidate domains for one string value.
struct CandidateKinds {
    items: [EmbeddedKind; 4],
    len: usize,
}

impl CandidateKinds {
    const fn new() -> Self {
        Self {
            items: [EmbeddedKind::Html; 4],
            len: 0,
        }
    }

    fn push(&mut self, kind: EmbeddedKind) {
        self.items[self.len] = kind;
        self.len += 1;
    }

    const fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn as_slice(&self) -> &[EmbeddedKind] {
        &self.items[..self.len]
    }
}

/// Effective character budget imposed by an enclosing string limit or parent
/// serialized JSON slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharBound {
    Unbounded,
    /// Decoded Unicode scalar count of the string value (`<= max`).
    Decoded(usize),
    /// Canonical JSON string literal scalar count including quotes (`<= max`).
    JsonLiteral(usize),
}

impl CharBound {
    fn allows(self, candidate: &str) -> bool {
        match self {
            Self::Unbounded => true,
            Self::Decoded(max) => candidate.chars().count() <= max,
            Self::JsonLiteral(max) => exact::encoded_string_char_len(candidate) <= max,
        }
    }

    const fn max_decoded_upper(self) -> Option<usize> {
        match self {
            Self::Unbounded => None,
            Self::Decoded(max) => Some(max),
            Self::JsonLiteral(max) => Some(max.saturating_sub(2)),
        }
    }

    fn tighten_decoded(self, doc_limit: Option<usize>) -> Self {
        let Some(limit) = doc_limit else {
            return self;
        };
        match self {
            Self::Unbounded => Self::Decoded(limit),
            Self::Decoded(current) => Self::Decoded(current.min(limit)),
            Self::JsonLiteral(current) => Self::JsonLiteral(current),
        }
    }
}

/// Leaf domains that contributed at least one rewrite inside a routed value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct RewriteDomains {
    pub(super) html: bool,
    pub(super) build_log: bool,
    pub(super) tabular: bool,
}

impl RewriteDomains {
    fn merge(&mut self, other: Self) {
        self.html |= other.html;
        self.build_log |= other.build_log;
        self.tabular |= other.tabular;
    }
}

/// Unique original leaf payload that must exist in Stash once an outer rewrite
/// is accepted.
#[derive(Debug, Clone)]
struct LeafPayload {
    digest: [u8; 32],
    value: Arc<str>,
}

/// One replacement for an embedded value.
#[derive(Debug, Clone)]
pub(super) struct Rewrite {
    pub(super) domains: RewriteDomains,
    pub(super) leaf_rewrites: usize,
    pub(super) max_inner_offset: usize,
    leaves: Arc<[LeafPayload]>,
    pub(super) value: String,
}

/// Per-document router shared by every candidate of one compression call.
///
/// A value seen again (a repeated field, or the same field in a later
/// candidate) reuses its first non-clipped outcome by fixed-size BLAKE3 digest,
/// so each unique payload is sniffed, rendered, and stashed at most once.
pub(super) struct EmbeddedRouter<'a> {
    stash: &'a dyn StashStore,
    recovery: &'a RecoveryMethod,
    enable_html: bool,
    enable_content: bool,
    enable_tabular: bool,
    build_log_mode: BuildLogMode,
    string_limit: Option<usize>,
    remaining_bytes: usize,
    remaining_renders: usize,
    negative_entries: usize,
    outcomes: HashMap<[u8; 32], Option<Rewrite>>,
    stashed_digests: HashSet<[u8; 32]>,
}

impl<'a> EmbeddedRouter<'a> {
    /// Builds a router when some kind is enabled and a rewrite could be
    /// recovered: a Stash is attached and the host declares a recovery path.
    pub(super) fn new(
        context: &JsonCompressionContext<'a>,
        string_limit: Option<usize>,
    ) -> Option<Self> {
        let enable_html = context.embedded_html && !context.is_error;
        let enable_content = context.embedded_content;
        if (!enable_html && !enable_content) || !context.recovery.is_available() {
            return None;
        }
        Some(Self {
            stash: context.stash?,
            recovery: context.recovery,
            enable_html,
            enable_content,
            enable_tabular: enable_content && !context.is_error,
            build_log_mode: if context.is_error {
                BuildLogMode::DiagnosticFailure
            } else {
                BuildLogMode::Success
            },
            string_limit,
            remaining_bytes: MAX_TOTAL_BYTES,
            remaining_renders: MAX_RENDERS,
            negative_entries: 0,
            outcomes: HashMap::new(),
            stashed_digests: HashSet::new(),
        })
    }

    /// Routes one string value when it matches an enabled embedded domain and
    /// clears the net-savings and outer string bounds, committing any newly
    /// required leaf payloads to Stash only after the outer rewrite is accepted.
    pub(super) fn route(
        &mut self,
        value: &str,
        depth: usize,
        max_outer_chars: Option<usize>,
        writes: &mut Vec<StashWrite>,
        errors: &mut usize,
    ) -> Option<Rewrite> {
        let bound = max_outer_chars.map_or(CharBound::Unbounded, CharBound::Decoded);
        let rewrite = self.route_bounded(value, depth, bound)?;

        let mut batch_writes: Vec<([u8; 32], StashWrite)> = Vec::new();
        for leaf in rewrite.leaves.iter() {
            if self.stashed_digests.contains(&leaf.digest) {
                continue;
            }
            match self.stash.stash(&leaf.value) {
                Ok(write) => batch_writes.push((leaf.digest, write)),
                Err(_) => {
                    *errors += 1;
                    for (_, created_write) in batch_writes {
                        if created_write.created {
                            let _ = self
                                .stash
                                .delete(&created_write.key, created_write.generation);
                        }
                    }
                    let outer_digest = compute_digest(value.as_bytes());
                    self.poison_failed_digest(outer_digest);
                    self.poison_failed_digest(leaf.digest);
                    return None;
                }
            }
        }
        for (digest, write) in batch_writes {
            self.stashed_digests.insert(digest);
            writes.push(write);
        }
        Some(rewrite)
    }

    fn cache_negative(&mut self, digest: [u8; 32]) {
        if self.negative_entries < MAX_NEGATIVE_CACHE_ENTRIES
            && !self.outcomes.contains_key(&digest)
        {
            self.outcomes.insert(digest, None);
            self.negative_entries += 1;
        }
    }

    fn poison_failed_digest(&mut self, digest: [u8; 32]) {
        if let Some(slot) = self.outcomes.get_mut(&digest) {
            if slot.is_some() {
                *slot = None;
                self.negative_entries += 1;
            }
        } else {
            self.cache_negative(digest);
        }
    }

    fn route_bounded(&mut self, value: &str, depth: usize, bound: CharBound) -> Option<Rewrite> {
        // Depth is checked before the cache so a value rewritten near the root
        // is still rejected when repeated past `MAX_DEPTH`.
        if depth > MAX_DEPTH
            || value.len() < MIN_CHARS
            || value.len() > MAX_VALUE_BYTES
            || is_already_embedded_view(value)
        {
            return None;
        }
        if value.len() < MIN_CHARS * 4 && value.chars().take(MIN_CHARS).count() < MIN_CHARS {
            return None;
        }

        let digest = compute_digest(value.as_bytes());
        if let Some(cached) = self.outcomes.get(&digest) {
            match cached {
                Some(rewrite) if depth.saturating_add(rewrite.max_inner_offset) <= MAX_DEPTH => {
                    return bound.allows(&rewrite.value).then(|| rewrite.clone());
                }
                Some(_) => {}
                None => return None,
            }
        }

        let candidates = self.sniff_candidates(value);
        if candidates.is_empty() {
            self.cache_negative(digest);
            return None;
        }
        if self.remaining_renders == 0 || value.len() > self.remaining_bytes {
            return None;
        }
        self.remaining_renders -= 1;
        self.remaining_bytes -= value.len();

        let mut clipped = false;
        let mut outcome = None;
        for &kind in candidates.as_slice() {
            match kind {
                EmbeddedKind::Json => {
                    let Ok(slots) = exact::scan_string_slots(value) else {
                        continue;
                    };
                    let (rewrite, json_clipped) = self.rewrite_json(value, &slots, depth, bound);
                    clipped |= json_clipped;
                    outcome = rewrite;
                    break;
                }
                EmbeddedKind::Html | EmbeddedKind::BuildLog | EmbeddedKind::Tabular => {
                    let (rewrite, leaf_clipped) = self.rewrite_leaf(kind, value, digest, bound);
                    clipped |= leaf_clipped;
                    if let Some(rewrite) = rewrite {
                        outcome = Some(rewrite);
                        break;
                    }
                }
            }
        }
        // Cache every positive non-depth-clipped rewrite, and cache negative
        // outcomes up to `MAX_NEGATIVE_CACHE_ENTRIES` unless clipped by depth
        // or a transient tighter char bound.
        if !clipped {
            match &outcome {
                Some(rewrite) => {
                    self.outcomes.insert(digest, Some(rewrite.clone()));
                }
                None => self.cache_negative(digest),
            }
        }
        outcome
    }

    fn sniff_candidates(&self, value: &str) -> CandidateKinds {
        let mut kinds = CandidateKinds::new();
        if self.enable_content && looks_like_json_container(value) {
            kinds.push(EmbeddedKind::Json);
        }
        if self.enable_html && HtmlExtractor.sniff_fragment(value) {
            kinds.push(EmbeddedKind::Html);
        }
        if self.enable_content && value.contains('\n') {
            if BuildLogCompressor::detect(value) {
                kinds.push(EmbeddedKind::BuildLog);
            }
            if self.enable_tabular && TabularCompressor::detect(value) {
                kinds.push(EmbeddedKind::Tabular);
            }
        }
        kinds
    }

    fn rewrite_leaf(
        &self,
        kind: EmbeddedKind,
        value: &str,
        digest: [u8; 32],
        bound: CharBound,
    ) -> (Option<Rewrite>, bool) {
        enum Prepared<H> {
            Html(H),
            BuildLog(String),
            Tabular(String),
        }

        let effective_bound = match kind {
            EmbeddedKind::Tabular => bound.tighten_decoded(self.string_limit),
            EmbeddedKind::Html | EmbeddedKind::BuildLog | EmbeddedKind::Json => bound,
        };
        // A tighter caller bound than the document-wide baseline may reject a
        // value here while a shallower/top-level occurrence still accepts it.
        let transient_bound = match kind {
            EmbeddedKind::Tabular => {
                let doc_default = self
                    .string_limit
                    .map_or(CharBound::Unbounded, CharBound::Decoded);
                effective_bound != doc_default
            }
            EmbeddedKind::Html | EmbeddedKind::BuildLog => effective_bound != CharBound::Unbounded,
            EmbeddedKind::Json => false,
        };

        let (prepared, domains) = match kind {
            EmbeddedKind::Html => {
                let Some(fragment) = HtmlExtractor.render_fragment(value) else {
                    return (None, false);
                };
                (
                    Prepared::Html(fragment),
                    RewriteDomains {
                        html: true,
                        ..RewriteDomains::default()
                    },
                )
            }
            EmbeddedKind::BuildLog => {
                let Some(compacted) =
                    BuildLogCompressor.render_embedded_view(value, self.build_log_mode)
                else {
                    return (None, false);
                };
                (
                    Prepared::BuildLog(compacted),
                    RewriteDomains {
                        build_log: true,
                        ..RewriteDomains::default()
                    },
                )
            }
            EmbeddedKind::Tabular => {
                // Tabular compaction removes quotes, CR bytes, leading BOM, and
                // blank separator lines while adding a header line, so unquoted
                // normalized characters establish a strict lower bound before
                // running the compressor.
                if let Some(max_chars) = effective_bound.max_decoded_upper()
                    && tabular_min_chars_lower_bound(value) > max_chars
                {
                    return (None, transient_bound);
                }
                let compacted = TabularCompressor.compress(value);
                if compacted.operations.is_empty() || compacted.output == value {
                    return (None, false);
                }
                (
                    Prepared::Tabular(compacted.output),
                    RewriteDomains {
                        tabular: true,
                        ..RewriteDomains::default()
                    },
                )
            }
            EmbeddedKind::Json => return (None, false),
        };

        let key = compute_key(value.as_bytes());
        let instruction = recovery_instruction(&key, self.recovery);
        let rewritten = match &prepared {
            Prepared::Html(view) => view.output(&instruction),
            Prepared::BuildLog(compacted) => {
                format!("{BUILD_LOG_VIEW_HEADER_PREFIX}{instruction}]\n{compacted}")
            }
            Prepared::Tabular(compacted) => {
                format!("{TABULAR_VIEW_HEADER_PREFIX}{instruction}]\n{compacted}")
            }
        };

        if !saves_enough(value, &rewritten) {
            return (None, false);
        }
        if !effective_bound.allows(&rewritten) {
            return (None, transient_bound);
        }
        (
            Some(Rewrite {
                domains,
                leaf_rewrites: 1,
                max_inner_offset: 0,
                leaves: Arc::from([LeafPayload {
                    digest,
                    value: Arc::from(value),
                }]),
                value: rewritten,
            }),
            false,
        )
    }

    fn rewrite_json(
        &mut self,
        value: &str,
        slots: &[StringSlot],
        depth: usize,
        bound: CharBound,
    ) -> (Option<Rewrite>, bool) {
        let effective_bound = bound.tighten_decoded(self.string_limit);
        let doc_default = self
            .string_limit
            .map_or(CharBound::Unbounded, CharBound::Decoded);
        let transient_bound = effective_bound != doc_default;
        let outer_max_chars = effective_bound.max_decoded_upper();

        let mut depth_clipped = false;
        let mut eligible = Vec::with_capacity(slots.len());
        let mut raw_char_lens = Vec::with_capacity(slots.len());
        let mut min_outer_chars = 0usize;
        let mut prev_end = 0usize;

        for slot in slots {
            min_outer_chars += value[prev_end..slot.start].chars().count();
            prev_end = slot.end;
            let raw_literal = &value[slot.start..slot.end];
            let raw_chars = raw_literal.chars().count();
            raw_char_lens.push(raw_chars);

            let relative_offset = 1usize.saturating_add(slot.inner_depth);
            let slot_depth = depth.saturating_add(relative_offset);
            if slot_depth > MAX_DEPTH {
                if slot.decoded.len() >= MIN_CHARS {
                    depth_clipped = true;
                }
                eligible.push(false);
                min_outer_chars += raw_chars;
                continue;
            }
            // Only rewrite slots that meet the char minimum, match at least one
            // enabled domain sniffer, and use canonical RFC 8259 escaping so
            // plain-text slots contribute their full `raw_chars` to
            // `min_outer_chars` upfront and restoring leaf Stash payloads
            // reproduces the outer serialized JSON string byte for byte.
            let is_eligible = slot.decoded.len() >= MIN_CHARS
                && slot.decoded.len() <= MAX_VALUE_BYTES
                && (slot.decoded.len() >= MIN_CHARS * 4
                    || slot.decoded.chars().take(MIN_CHARS).count() >= MIN_CHARS)
                && !is_already_embedded_view(&slot.decoded)
                && exact::is_canonical_string_literal(raw_literal, &slot.decoded)
                && !self.sniff_candidates(&slot.decoded).is_empty();
            eligible.push(is_eligible);
            min_outer_chars += if is_eligible { 2 } else { raw_chars };
        }
        min_outer_chars += value[prev_end..].chars().count();

        if let Some(limit) = outer_max_chars
            && min_outer_chars > limit
        {
            return (None, depth_clipped || transient_bound);
        }

        let mut limit_clipped = false;
        let mut rewritten = String::new();
        let mut cursor = 0usize;
        let mut domains = RewriteDomains::default();
        let mut leaf_rewrites = 0usize;
        let mut max_inner_offset = 0usize;
        let mut leaves: Vec<LeafPayload> = Vec::new();
        let mut leaf_digests: HashSet<[u8; 32]> = HashSet::new();

        for ((slot, &is_eligible), &raw_chars) in slots.iter().zip(&eligible).zip(&raw_char_lens) {
            if !is_eligible {
                continue;
            }
            let relative_offset = 1usize.saturating_add(slot.inner_depth);
            let slot_depth = depth.saturating_add(relative_offset);
            let inner_bound = match outer_max_chars {
                Some(limit) => {
                    let other_min = min_outer_chars.saturating_sub(2);
                    if other_min > limit {
                        limit_clipped |= transient_bound;
                        break;
                    }
                    CharBound::JsonLiteral(limit - other_min)
                }
                None => CharBound::Unbounded,
            };

            let Some(inner) = self.route_bounded(&slot.decoded, slot_depth, inner_bound) else {
                min_outer_chars = min_outer_chars.saturating_sub(2).saturating_add(raw_chars);
                if let Some(limit) = outer_max_chars
                    && min_outer_chars > limit
                {
                    limit_clipped |= transient_bound;
                    break;
                }
                continue;
            };

            let encoded = exact::encode_string_literal(&inner.value);
            let encoded_chars = encoded.chars().count();
            min_outer_chars = min_outer_chars
                .saturating_sub(2)
                .saturating_add(encoded_chars);

            if leaf_rewrites == 0 {
                rewritten.reserve(value.len());
            }
            rewritten.push_str(&value[cursor..slot.start]);
            rewritten.push_str(&encoded);
            cursor = slot.end;
            domains.merge(inner.domains);
            leaf_rewrites += inner.leaf_rewrites;
            max_inner_offset =
                max_inner_offset.max(relative_offset.saturating_add(inner.max_inner_offset));
            for leaf in inner.leaves.iter() {
                if leaf_digests.insert(leaf.digest) {
                    leaves.push(leaf.clone());
                }
            }
        }

        if leaf_rewrites == 0 {
            return (None, depth_clipped || limit_clipped);
        }
        rewritten.push_str(&value[cursor..]);
        if !effective_bound.allows(&rewritten) {
            return (None, depth_clipped || transient_bound);
        }
        if !strictly_smaller_in_json(value, &rewritten) {
            return (None, depth_clipped);
        }
        (
            Some(Rewrite {
                domains,
                leaf_rewrites,
                max_inner_offset,
                leaves: Arc::from(leaves),
                value: rewritten,
            }),
            depth_clipped || limit_clipped,
        )
    }
}

fn is_already_embedded_view(value: &str) -> bool {
    value.starts_with(HTML_VIEW_HEADER_PREFIX)
        || value.starts_with(BUILD_LOG_VIEW_HEADER_PREFIX)
        || value.starts_with(TABULAR_VIEW_HEADER_PREFIX)
}

fn tabular_min_chars_lower_bound(value: &str) -> usize {
    let body = value.strip_prefix('\u{feff}').unwrap_or(value);
    let mut count = 0usize;
    let mut saw_line_char = false;
    let mut pending_newline = false;
    for ch in body.chars() {
        match ch {
            '"' | '\r' => {}
            '\n' => {
                if saw_line_char {
                    pending_newline = true;
                    saw_line_char = false;
                }
            }
            _ => {
                if pending_newline {
                    count += 1;
                    pending_newline = false;
                }
                count += 1;
                saw_line_char = true;
            }
        }
    }
    BUILD_LOG_OR_TABULAR_MIN_HEADER_CHARS + count
}

const BUILD_LOG_OR_TABULAR_MIN_HEADER_CHARS: usize = TABULAR_VIEW_HEADER_PREFIX.len() + 2;

fn looks_like_json_container(value: &str) -> bool {
    let trimmed = value.trim_matches(|c: char| c.is_ascii_whitespace());
    let bytes = trimmed.as_bytes();
    match (bytes.first(), bytes.last()) {
        (Some(b'{'), Some(b'}')) => bytes[1..]
            .iter()
            .find(|b| !b.is_ascii_whitespace())
            .is_some_and(|b| matches!(b, b'"' | b'}')),
        (Some(b'['), Some(b']')) => bytes[1..]
            .iter()
            .find(|b| !b.is_ascii_whitespace())
            .is_some_and(|b| {
                matches!(
                    b,
                    b']' | b'{' | b'[' | b'"' | b'-' | b'0'..=b'9' | b't' | b'f' | b'n'
                )
            }),
        _ => false,
    }
}

fn strictly_smaller_in_json(original: &str, replacement: &str) -> bool {
    exact::encoded_string_char_len(replacement) < exact::encoded_string_char_len(original)
        && estimate_tokens(&exact::encode_string_literal(replacement))
            < estimate_tokens(&exact::encode_string_literal(original))
}

/// Whether `replacement` saves at least [`MIN_TOKEN_SAVINGS`] estimated
/// tokens once serialized back into a JSON string literal.
fn saves_enough(original: &str, replacement: &str) -> bool {
    if exact::encoded_string_char_len(replacement) >= exact::encoded_string_char_len(original) {
        return false;
    }
    let original_encoded = exact::encode_string_literal(original);
    let replacement_encoded = exact::encode_string_literal(replacement);
    estimate_tokens(&replacement_encoded).saturating_add(MIN_TOKEN_SAVINGS)
        <= estimate_tokens(&original_encoded)
}

#[cfg(test)]
mod tests;
