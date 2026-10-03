//! Opt-in OS-adapter transform stage for `SKILL.md`.
//!
//! Rewrites distribution-specific literal strings (package managers, package
//! names, service unit names, filesystem paths) between Ubuntu/Debian and
//! Alinux/Anolis style using a versioned rule artifact.
//!
//! SkillFS ships a built-in Ubuntu/Alinux catalog, embedded in the binary from
//! the repository asset (`assets/ubuntu-alinux.yaml`), and loads it via
//! [`OsAdapterStage::load_default`] when the adapter is enabled without an
//! explicit path. An operator may instead point at an external, read-only rule
//! artifact via [`OsAdapterStage::load`] to override the default. Both are the
//! same format: a top-level YAML sequence of rules. The adapter stays disabled
//! by default; enabling it and choosing built-in versus external are the only
//! decisions.
//!
//! # Rule schema
//!
//! Each list entry is a mapping with these fields:
//!
//! ```yaml
//! - ubuntu: "apt-get install -y "     # literal source on the Ubuntu/Debian side
//!   alinux: "dnf install -y "         # literal source on the Alinux/Anolis side
//!   direction: bidirectional          # bidirectional | ubuntu_to_alinux_only | alinux_to_ubuntu_only
//!   match: literal                    # OPTIONAL literal | token; default literal
//!   auto_apply: always                # always | never (explicit eligibility)
//!   confidence: high                  # OPTIONAL, ignored by SkillFS
//!   notes: "..."                      # OPTIONAL, ignored by SkillFS
//! ```
//!
//! Eligibility is governed **only** by the explicit `auto_apply` field: a rule
//! is applied automatically iff `auto_apply = always`. `confidence` and `notes`
//! are accepted as human annotations but carry no behavior — SkillFS does not
//! inherit any confidence-driven semantics.
//! `match` defaults to `literal`; `token` requires ASCII-alphanumeric
//! boundaries at alphanumeric source edges after direction resolution.
//!
//! # Determinism and validation
//!
//! The rule artifact is parsed, validated, and compiled into an ordered
//! substitution table exactly once, when the mount starts. Validation rejects
//! unknown fields, invalid `direction`/`auto_apply`/`match` values, empty
//! patterns, and duplicate or ambiguous source patterns for the resolved target.
//! The per-read hot path performs a single left-to-right pass over the original
//! bytes, choosing the longest matching source pattern at each position (most
//! specific wins) and skipping past it — so overlapping patterns never cascade
//! and rule file order does not affect the result.
//!
//! Ineligible patterns (`auto_apply: never`, identity where the two sides are
//! equal, or direction-disallowed for the resolved target) still take part in
//! the scan as *protection* matches: when one is the longest match it is emitted
//! unchanged and skipped, so a shorter eligible rule cannot rewrite inside a
//! span such a rule claims. Protection deduplication uses `(source, match)`:
//! an eligible substitution removes protection only for the same source and
//! match mode. Different modes coexist; a substitution takes precedence only
//! when its own mode matches the current input, otherwise a matching protection
//! still preserves the span. This holds for external override artifacts too.
//!
//! # Module layout
//!
//! - `detect` — target-OS selection and `/etc/os-release` detection.
//! - `error` — the owned [`OsAdapterError`] type.
//! - `rules` — schema, validation, and substitution compilation.
//! - `stage` — the [`OsAdapterStage`] transform stage.

mod detect;
mod error;
mod rules;
mod stage;

pub use detect::{OsTarget, TargetSelector, parse_os_release};
pub use error::OsAdapterError;
pub use stage::OsAdapterStage;
