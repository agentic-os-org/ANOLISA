//! Source drift observation (visibility only).
//!
//! Background. SkillFS only routes filesystem operations through its policy
//! and audit layers when those operations go through the FUSE mountpoint.
//! In a non-in-place mount the physical source path is still directly
//! reachable, so a process that knows the source path can write or delete
//! `SKILL.md` and other files without SkillFS observing the change. Even in
//! an in-place mount, processes that already had file descriptors open
//! before the over-mount started can keep writing into the underlying
//! inodes outside FUSE.
//!
//! This module gives a small, well-separated seam for *describing* such
//! out-of-band changes as normalized [`SkillEvent`] records of kind
//! [`SkillEventKind::SourceChanged`]. It performs **no** enforcement,
//! quarantine, lifecycle behavior, or real-time blocking — its job is to
//! turn a "something happened in the source tree" observation into a
//! consistent audit record that downstream code can ingest through the
//! existing [`SkillEventSink`] / [`super::audit::JsonlFileAuditSink`]
//! pipeline.
//!
//! Default behavior is **no-op**. Nothing in the runtime currently feeds
//! drift observations to a [`SourceDriftObserver`]; the type is exposed so
//! tests, future watcher wiring, and external integrations can plug in
//! without changing FUSE callbacks. POSIX errno paths, compiled `SKILL.md`
//! semantics, `.skill-meta` enforcement, and the audit JSONL field shape
//! are unchanged by this module.
//!
//! Scope discipline (intentionally out of scope here):
//!
//! * trusted-writer identity / process attribution;
//! * skill-ledger allowlists / capability enforcement;
//! * symlink, hardlink, xattr, `mknod`, `fallocate`, `lseek`,
//!   `copy_file_range`;
//! * lifecycle namespaces;
//! * broad watcher hot sync (the existing `skillfs-core::watcher` is still
//!   not wired into the runtime — this module only normalizes input).
//!
//! Layout caveat. The lexical classifier reliably covers the **flat**
//! SkillFS layout (`<source>/<skill>/...`). The **categorized** layout
//! (`<source>/<category>/<skill>/SKILL.md`, supported by
//! `skillfs-core::store::is_category_dir`) cannot be told apart from a
//! flat layout without filesystem context. The classifier therefore:
//!
//! * refuses skill attribution for any `<source>/<a>/<b>/.../SKILL.md`
//!   that is not at depth 1 — those route to
//!   [`DriftScope::InsideSourceOutsideSkill`] so an audit consumer is
//!   not misled by a fabricated `skill=<category>` field;
//! * still applies flat-layout attribution to non-manifest sub-paths
//!   (`<source>/<a>/<b>/<c>` becomes
//!   [`DriftScope::InsideSkill`] with skill=`a`); producers with store
//!   context (e.g. a future watcher integration) should pre-classify
//!   through [`DriftEvent::with_scope`] using the skill's recorded
//!   `source_path`.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use super::event::{NoopEventSink, SkillEvent, SkillEventAction, SkillEventKind, SkillEventSink};

/// Kind of change observed in the source tree.
///
/// Mirrors the high-level distinctions surfaced by `notify`-style watchers
/// without coupling to that crate. `Unknown` is the deliberate
/// "we cannot tell" bucket so callers do not have to invent an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftChangeKind {
    Created,
    Modified,
    Deleted,
    Renamed,
    /// The change kind could not be determined (e.g. a coalesced batch, an
    /// unsupported event, or an external producer that does not surface a
    /// kind).
    Unknown,
}

impl DriftChangeKind {
    /// Lowercase, snake_case label for the change kind. Used by the
    /// `detail` field of the emitted [`SkillEvent`] so downstream JSONL
    /// consumers can grep on a stable string.
    pub fn as_str(self) -> &'static str {
        match self {
            DriftChangeKind::Created => "created",
            DriftChangeKind::Modified => "modified",
            DriftChangeKind::Deleted => "deleted",
            DriftChangeKind::Renamed => "renamed",
            DriftChangeKind::Unknown => "unknown",
        }
    }
}

/// Where a drift observation lands relative to the SkillFS source tree.
///
/// Determined by lexical comparison against the configured source root;
/// no syscalls are performed. Ambiguous inputs (paths that cannot be
/// stripped against the source root, paths whose first component is `.`
/// or `..`) collapse into [`DriftScope::Unknown`] rather than guessing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriftScope {
    /// `<source>/<skill>/SKILL.md` — a manifest changed.
    SkillMd { skill_name: String },
    /// `<source>/<skill>` — the skill directory itself was created or
    /// removed (no relative path under the skill).
    SkillDir { skill_name: String },
    /// `<source>/<skill>/<rel>` for some `<rel>` other than `SKILL.md`.
    InsideSkill {
        skill_name: String,
        relative_path: PathBuf,
    },
    /// Path is under the source tree but not under any skill directory
    /// (e.g. `skillfs-views.toml` at the source root, or a top-level
    /// file/directory that is not a skill).
    InsideSourceOutsideSkill { relative_path: PathBuf },
    /// Path is outside the source tree (absolute target that does not
    /// start with the source root, or a relative target that escapes via
    /// `..`).
    OutsideSource,
    /// The classifier could not lexically resolve the input — for example
    /// an empty path, a non-absolute source root, or a `..` past the
    /// source root.
    Unknown,
}

impl DriftScope {
    /// Skill name when the scope identifies one.
    pub fn skill_name(&self) -> Option<&str> {
        match self {
            DriftScope::SkillMd { skill_name }
            | DriftScope::SkillDir { skill_name }
            | DriftScope::InsideSkill { skill_name, .. } => Some(skill_name.as_str()),
            DriftScope::InsideSourceOutsideSkill { .. }
            | DriftScope::OutsideSource
            | DriftScope::Unknown => None,
        }
    }

    /// Skill-relative path when the scope identifies one.
    ///
    /// Returns `Some(Path::new("SKILL.md"))` for [`DriftScope::SkillMd`] so
    /// the emitted [`SkillEvent`] preserves the same shape as
    /// `SKILL.md`-targeting events emitted by FUSE callbacks.
    pub fn relative_path(&self) -> Option<&Path> {
        match self {
            DriftScope::SkillMd { .. } => Some(Path::new("SKILL.md")),
            DriftScope::InsideSkill { relative_path, .. } => Some(relative_path.as_path()),
            DriftScope::SkillDir { .. }
            | DriftScope::InsideSourceOutsideSkill { .. }
            | DriftScope::OutsideSource
            | DriftScope::Unknown => None,
        }
    }

    /// Stable, lowercase label used in the emitted event's `detail` field
    /// so JSONL consumers can distinguish ambiguous from precise scopes
    /// without reconstructing the path themselves.
    pub fn as_str(&self) -> &'static str {
        match self {
            DriftScope::SkillMd { .. } => "skill_md",
            DriftScope::SkillDir { .. } => "skill_dir",
            DriftScope::InsideSkill { .. } => "inside_skill",
            DriftScope::InsideSourceOutsideSkill { .. } => "inside_source_outside_skill",
            DriftScope::OutsideSource => "outside_source",
            DriftScope::Unknown => "unknown",
        }
    }
}

/// Lexically classify an `observed` path against the SkillFS `source_root`.
///
/// Both paths are interpreted as-is — no canonicalization, no symlink
/// resolution, no syscalls. Callers that need to compare canonical paths
/// must resolve them themselves before calling this helper. The classifier
/// returns [`DriftScope::Unknown`] rather than attempting to guess for
/// inputs it cannot lexically reason about.
pub fn classify_drift_path(source_root: &Path, observed: &Path) -> DriftScope {
    if source_root.as_os_str().is_empty() || observed.as_os_str().is_empty() {
        return DriftScope::Unknown;
    }

    let normalized_source = match normalize_lexical(source_root) {
        Some(p) => p,
        None => return DriftScope::Unknown,
    };
    let normalized_observed = match normalize_lexical(observed) {
        Some(p) => p,
        None => return DriftScope::Unknown,
    };

    let after_source = match normalized_observed.strip_prefix(&normalized_source) {
        Ok(rel) => rel,
        Err(_) => return DriftScope::OutsideSource,
    };

    let mut comps = after_source.components();
    let first = match comps.next() {
        Some(Component::Normal(name)) => name,
        // The observed path is the source root itself, or starts with a
        // non-Normal component (e.g. `RootDir`). Either way it is inside
        // the source tree but not under any skill.
        None => {
            return DriftScope::InsideSourceOutsideSkill {
                relative_path: PathBuf::new(),
            };
        }
        Some(_) => {
            return DriftScope::InsideSourceOutsideSkill {
                relative_path: after_source.to_path_buf(),
            };
        }
    };

    let skill_name = first.to_string_lossy().into_owned();
    // Reject path components that cannot be SkillFS skill directories.
    // The shape rule must match the canonical validator in
    // `skillfs-core::parser::validate_name`: kebab-case
    // (`[a-z0-9-]+`), no leading or trailing hyphen, length ≤ 64.
    // Anything else — uppercase letters (`Alpha`), underscores
    // (`foo_bar`), embedded dots (`skillfs-views.toml`), leading dots
    // (`.staging`), or out-of-spec lengths — routes to
    // [`DriftScope::InsideSourceOutsideSkill`] so audit consumers do
    // not see fabricated `skill=…` attribution. Genuine skill
    // directories that accidentally violate the convention also lose
    // attribution here; that is intentional, since the lexical
    // classifier cannot distinguish them from arbitrary top-level
    // entries without filesystem context.
    if !looks_like_skill_name_component(&skill_name) {
        return DriftScope::InsideSourceOutsideSkill {
            relative_path: after_source.to_path_buf(),
        };
    }

    let rest: PathBuf = comps.as_path().to_path_buf();
    if rest.as_os_str().is_empty() {
        return DriftScope::SkillDir { skill_name };
    }
    if rest.as_os_str() == std::ffi::OsStr::new("SKILL.md") {
        return DriftScope::SkillMd { skill_name };
    }
    // Categorized-layout safeguard. SkillFS supports
    // `<source>/<category>/<skill>/SKILL.md` as a first-class layout
    // (see `skillfs-core::store::is_category_dir`). When we see a
    // manifest at depth ≥ 2 (i.e. `<source>/<a>/<b>/.../SKILL.md`)
    // the lexical classifier cannot tell whether `a` is a flat skill
    // with a stray nested manifest or a category whose real skill is
    // `b`. Either guess produces a misleading audit record:
    // `skill=a, path=b/SKILL.md` invents an attribution that does not
    // exist in store keys. Refuse skill attribution and surface the
    // observation as inside-source-outside-skill so a downstream
    // consumer with store context (e.g. a future watcher integration)
    // can re-attribute via [`DriftEvent::with_scope`] using the
    // `source_path` recorded by `SkillStore`.
    if rest_ends_with_skill_md(&rest) {
        return DriftScope::InsideSourceOutsideSkill {
            relative_path: after_source.to_path_buf(),
        };
    }
    // Flat-layout best effort. This still misattributes deeper paths
    // under a categorized layout (e.g. `tools/alpha/notes.txt` will
    // bucket as `skill=tools, path=alpha/notes.txt`). Producers that
    // can disambiguate via the loaded `SkillStore` should pre-classify
    // through [`DriftEvent::with_scope`]; the lexical classifier is
    // documented as reliable only for the flat layout.
    DriftScope::InsideSkill {
        skill_name,
        relative_path: rest,
    }
}

/// Pure-lexical skill-name component check, mirroring the canonical
/// validator in `skillfs-core::parser::validate_name`.
///
/// Returns `true` only for non-empty, kebab-case ASCII identifiers up to
/// 64 chars, with no leading/trailing hyphen. Everything else (uppercase,
/// underscores, dots, length > 64, hyphen edges) is rejected so the drift
/// classifier does not invent skill attribution for entries that the
/// store/parser would not load as a skill.
fn looks_like_skill_name_component(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    if name.starts_with('-') || name.ends_with('-') {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// True when the trailing component of `rest` is exactly `SKILL.md`.
fn rest_ends_with_skill_md(rest: &Path) -> bool {
    rest.file_name()
        .map(|n| n == std::ffi::OsStr::new("SKILL.md"))
        .unwrap_or(false)
}

/// Lexical normalization of `.` and `..` components without touching the
/// filesystem. Returns `None` when `..` would escape the absolute root or
/// when an unsupported `Prefix` component (Windows) is hit.
fn normalize_lexical(path: &Path) -> Option<PathBuf> {
    let mut out: Vec<Component> = Vec::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) => return None,
                Some(Component::Prefix(_)) => return None,
                Some(Component::ParentDir) | Some(Component::CurDir) | None => {
                    out.push(Component::ParentDir);
                }
            },
            other => out.push(other),
        }
    }
    Some(out.iter().collect())
}

/// A normalized drift observation.
///
/// Carries everything needed to convert into a [`SkillEvent`] of kind
/// [`SkillEventKind::SourceChanged`] without re-running the classifier.
/// Construction is cheap; emission is decoupled so callers can pre-classify
/// in one place and emit somewhere else.
#[derive(Debug, Clone)]
pub struct DriftEvent {
    pub change_kind: DriftChangeKind,
    pub scope: DriftScope,
    /// Original observed path, preserved verbatim for the `detail` field.
    /// Useful when [`DriftScope`] is `OutsideSource` or `Unknown` and the
    /// skill/relative_path are absent.
    pub original_path: PathBuf,
}

impl DriftEvent {
    /// Build a drift event by lexically classifying `observed_path`
    /// against `source_root`.
    pub fn classify(
        source_root: &Path,
        observed_path: impl Into<PathBuf>,
        change_kind: DriftChangeKind,
    ) -> Self {
        let original_path = observed_path.into();
        let scope = classify_drift_path(source_root, &original_path);
        Self {
            change_kind,
            scope,
            original_path,
        }
    }

    /// Build a drift event from an explicit, already-classified scope.
    /// Provided for callers that resolve drift through a different path
    /// (e.g. an external watcher API that already attributes to a skill).
    pub fn with_scope(
        change_kind: DriftChangeKind,
        scope: DriftScope,
        original_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            change_kind,
            scope,
            original_path: original_path.into(),
        }
    }

    /// Convert the observation into a normalized [`SkillEvent`].
    ///
    /// The returned event is always of kind
    /// [`SkillEventKind::SourceChanged`] with action
    /// [`SkillEventAction::Allowed`] (drift is observed, not gated).
    /// `skill` and `path` come from the scope when derivable; otherwise
    /// they are omitted and the original path is preserved in `detail`
    /// alongside the change-kind / scope labels.
    pub fn to_skill_event(&self) -> SkillEvent {
        let mut event = SkillEvent::new(SkillEventKind::SourceChanged)
            .with_action(SkillEventAction::Allowed)
            .with_detail(self.detail_string());
        if let Some(skill) = self.scope.skill_name() {
            event = event.with_skill_name(skill);
        }
        if let Some(rel) = self.scope.relative_path() {
            event = event.with_relative_path(rel);
        }
        event
    }

    fn detail_string(&self) -> String {
        // Stable shape: `change=<kind> scope=<scope> path=<observed>`.
        // The observed path is included so consumers can recover the raw
        // input even when scope is OutsideSource / Unknown. The display
        // form may contain `=` and spaces; that is acceptable because
        // detail is documented as free-form text, not a structured field.
        format!(
            "change={} scope={} path={}",
            self.change_kind.as_str(),
            self.scope.as_str(),
            self.original_path.display()
        )
    }
}

/// Observer that turns out-of-band source changes into [`SkillEvent`]
/// records and emits them through an injected [`SkillEventSink`].
///
/// The observer holds an `Arc<dyn SkillEventSink>` so the same sink used
/// for FUSE-side audit (`JsonlFileAuditSink`, `InMemoryEventSink`, etc.)
/// can absorb drift records too without a second pipeline. The default
/// sink is [`NoopEventSink`] so an unconfigured observer is genuinely a
/// no-op; nothing is logged unless a non-default sink is attached.
///
/// Construction is cheap and infallible; nothing in this module reads or
/// writes to disk. The observer is not wired into the FUSE runtime and
/// has no effect on filesystem operations until a producer (a future
/// watcher integration, a test, or an external integration) calls one of
/// its `observe_*` methods.
pub struct SourceDriftObserver {
    source_root: PathBuf,
    sink: Arc<dyn SkillEventSink>,
}

impl SourceDriftObserver {
    /// Build an observer that emits through `sink`. Use this when
    /// integrating with a real audit pipeline (e.g. share an
    /// `Arc<JsonlFileAuditSink>` with the FUSE side).
    pub fn new(source_root: impl Into<PathBuf>, sink: Arc<dyn SkillEventSink>) -> Self {
        Self {
            source_root: source_root.into(),
            sink,
        }
    }

    /// Build an observer with the default [`NoopEventSink`]. Useful when
    /// the runtime wires the observer in unconditionally but audit logging
    /// has not been turned on.
    pub fn no_op(source_root: impl Into<PathBuf>) -> Self {
        Self::new(source_root, Arc::new(NoopEventSink))
    }

    /// Observe a single drift change and emit it as a [`SkillEvent`].
    ///
    /// Returns the [`DriftEvent`] that was constructed so callers can
    /// inspect or log it. The sink is invoked synchronously — best-effort
    /// behavior is the responsibility of the sink implementation, exactly
    /// as for FUSE-side emission.
    pub fn observe(&self, observed_path: &Path, change_kind: DriftChangeKind) -> DriftEvent {
        let drift = DriftEvent::classify(&self.source_root, observed_path, change_kind);
        self.sink.emit(&drift.to_skill_event());
        drift
    }

    /// Observe a pre-classified drift change. Provided for producers that
    /// already know the [`DriftScope`] and would lose information by
    /// re-running the lexical classifier (for example a watcher that
    /// attributes via a different mechanism).
    pub fn observe_event(&self, drift: &DriftEvent) {
        self.sink.emit(&drift.to_skill_event());
    }

    /// Source root the observer was built against.
    pub fn source_root(&self) -> &Path {
        &self.source_root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::event::{InMemoryEventSink, SkillEventAction, SkillEventKind};
    use std::path::Path;

    #[test]
    fn change_kind_strings_are_stable() {
        assert_eq!(DriftChangeKind::Created.as_str(), "created");
        assert_eq!(DriftChangeKind::Modified.as_str(), "modified");
        assert_eq!(DriftChangeKind::Deleted.as_str(), "deleted");
        assert_eq!(DriftChangeKind::Renamed.as_str(), "renamed");
        assert_eq!(DriftChangeKind::Unknown.as_str(), "unknown");
    }

    #[test]
    fn classify_skill_md_recognizes_manifest_path() {
        let source = Path::new("/srv/skills");
        let scope = classify_drift_path(source, Path::new("/srv/skills/alpha/SKILL.md"));
        assert_eq!(
            scope,
            DriftScope::SkillMd {
                skill_name: "alpha".to_string()
            }
        );
        assert_eq!(scope.skill_name(), Some("alpha"));
        assert_eq!(scope.relative_path(), Some(Path::new("SKILL.md")));
        assert_eq!(scope.as_str(), "skill_md");
    }

    #[test]
    fn classify_skill_dir_when_path_is_skill_root() {
        let source = Path::new("/srv/skills");
        let scope = classify_drift_path(source, Path::new("/srv/skills/alpha"));
        assert_eq!(
            scope,
            DriftScope::SkillDir {
                skill_name: "alpha".to_string()
            }
        );
        assert_eq!(scope.skill_name(), Some("alpha"));
        assert_eq!(scope.relative_path(), None);
    }

    #[test]
    fn classify_inside_skill_for_subpath() {
        let source = Path::new("/srv/skills");
        let scope = classify_drift_path(source, Path::new("/srv/skills/alpha/scripts/run.sh"));
        match &scope {
            DriftScope::InsideSkill {
                skill_name,
                relative_path,
            } => {
                assert_eq!(skill_name, "alpha");
                assert_eq!(relative_path, Path::new("scripts/run.sh"));
            }
            other => panic!("expected InsideSkill, got {other:?}"),
        }
        assert_eq!(scope.skill_name(), Some("alpha"));
        assert_eq!(scope.relative_path(), Some(Path::new("scripts/run.sh")));
    }

    #[test]
    fn classify_inside_source_outside_skill_at_root() {
        let source = Path::new("/srv/skills");
        let scope = classify_drift_path(source, Path::new("/srv/skills/skillfs-views.toml"));
        match &scope {
            DriftScope::InsideSourceOutsideSkill { relative_path } => {
                assert_eq!(relative_path, Path::new("skillfs-views.toml"));
            }
            other => panic!("expected InsideSourceOutsideSkill, got {other:?}"),
        }
        assert_eq!(scope.skill_name(), None);
    }

    #[test]
    fn classify_rejects_non_kebab_top_level_components() {
        // Mirror the validator in `skillfs-core::parser::validate_name`:
        // anything that wouldn't load as a skill must not be attributed
        // as one. These are the failure modes the reviewer flagged
        // (uppercase, underscore, hyphen edges, > 64 chars).
        let source = Path::new("/srv/skills");
        let bad_names = [
            "Alpha",
            "ALPHA",
            "foo_bar",
            "-bad",
            "bad-",
            "alpha!",
            "alpha space",
            // 65-char string of valid chars is still rejected on length.
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ];
        for name in bad_names {
            let path = format!("/srv/skills/{name}/SKILL.md");
            let scope = classify_drift_path(source, Path::new(&path));
            match &scope {
                DriftScope::InsideSourceOutsideSkill { relative_path } => {
                    assert_eq!(
                        relative_path,
                        Path::new(&format!("{name}/SKILL.md")),
                        "bad name `{name}` should not steal skill attribution"
                    );
                }
                other => panic!("expected InsideSourceOutsideSkill for `{name}`, got {other:?}"),
            }
            assert_eq!(
                scope.skill_name(),
                None,
                "bad name `{name}` must not surface skill attribution"
            );
        }
    }

    #[test]
    fn classify_accepts_kebab_case_skill_names() {
        let source = Path::new("/srv/skills");
        let good_names = ["alpha", "web-search", "alpha2", "1-second", "a"];
        for name in good_names {
            let path = format!("/srv/skills/{name}/SKILL.md");
            let scope = classify_drift_path(source, Path::new(&path));
            assert_eq!(
                scope,
                DriftScope::SkillMd {
                    skill_name: name.to_string()
                },
                "good name `{name}` should classify as SkillMd"
            );
        }
    }

    #[test]
    fn classify_refuses_attribution_for_categorized_manifest_path() {
        // SkillFS supports a categorized layout
        // `<source>/<category>/<skill>/SKILL.md`. Lexically we cannot
        // tell that from a flat layout's stray nested SKILL.md, and a
        // wrong guess (`skill=<category>, path=<skill>/SKILL.md`) would
        // invent an attribution that does not exist in the store. Pin
        // the safe fallback: refuse skill attribution and surface the
        // observation as inside-source-outside-skill so a future
        // watcher integration with store context can re-attribute via
        // `DriftEvent::with_scope`.
        let source = Path::new("/srv/skills");
        let scope = classify_drift_path(source, Path::new("/srv/skills/tools/alpha/SKILL.md"));
        match &scope {
            DriftScope::InsideSourceOutsideSkill { relative_path } => {
                assert_eq!(relative_path, Path::new("tools/alpha/SKILL.md"));
            }
            other => panic!("expected InsideSourceOutsideSkill, got {other:?}"),
        }
        assert_eq!(scope.skill_name(), None);

        // Even deeper nesting must not invent attribution either.
        let scope =
            classify_drift_path(source, Path::new("/srv/skills/tools/inner/alpha/SKILL.md"));
        assert!(
            matches!(scope, DriftScope::InsideSourceOutsideSkill { .. }),
            "expected InsideSourceOutsideSkill for nested manifest, got {scope:?}"
        );
    }

    #[test]
    fn classify_inside_source_outside_skill_for_dotfile() {
        // Hidden top-level entries are not skills; they bucket as
        // inside-source-outside-skill rather than `SkillDir { ".staging" }`.
        let source = Path::new("/srv/skills");
        let scope = classify_drift_path(source, Path::new("/srv/skills/.staging/foo"));
        match &scope {
            DriftScope::InsideSourceOutsideSkill { relative_path } => {
                assert_eq!(relative_path, Path::new(".staging/foo"));
            }
            other => panic!("expected InsideSourceOutsideSkill, got {other:?}"),
        }
    }

    #[test]
    fn classify_outside_source_for_unrelated_absolute() {
        let source = Path::new("/srv/skills");
        let scope = classify_drift_path(source, Path::new("/etc/passwd"));
        assert_eq!(scope, DriftScope::OutsideSource);
        assert_eq!(scope.skill_name(), None);
        assert_eq!(scope.relative_path(), None);
    }

    #[test]
    fn classify_outside_source_for_parent_traversal() {
        let source = Path::new("/srv/skills");
        let scope = classify_drift_path(source, Path::new("/srv/skills/alpha/../../etc"));
        assert_eq!(scope, DriftScope::OutsideSource);
    }

    #[test]
    fn classify_unknown_for_empty_inputs() {
        assert_eq!(
            classify_drift_path(Path::new(""), Path::new("/srv/skills/alpha")),
            DriftScope::Unknown
        );
        assert_eq!(
            classify_drift_path(Path::new("/srv/skills"), Path::new("")),
            DriftScope::Unknown
        );
    }

    #[test]
    fn classify_normalizes_curdir_and_parent_segments() {
        // /srv/skills/./alpha/./SKILL.md should normalize to the manifest
        // form rather than landing in InsideSkill or Unknown.
        let source = Path::new("/srv/skills");
        let scope = classify_drift_path(source, Path::new("/srv/skills/./alpha/./SKILL.md"));
        assert_eq!(
            scope,
            DriftScope::SkillMd {
                skill_name: "alpha".to_string()
            }
        );
    }

    #[test]
    fn drift_event_to_skill_event_populates_skill_and_path() {
        let source = Path::new("/srv/skills");
        let drift = DriftEvent::classify(
            source,
            Path::new("/srv/skills/alpha/SKILL.md"),
            DriftChangeKind::Modified,
        );
        let event = drift.to_skill_event();
        assert_eq!(event.kind, SkillEventKind::SourceChanged);
        assert_eq!(event.action, Some(SkillEventAction::Allowed));
        assert_eq!(event.skill_name.as_deref(), Some("alpha"));
        assert_eq!(event.relative_path.as_deref(), Some(Path::new("SKILL.md")));
        let detail = event.detail.as_deref().expect("detail must be set");
        assert!(detail.contains("change=modified"), "{detail}");
        assert!(detail.contains("scope=skill_md"), "{detail}");
        assert!(
            detail.contains("path=/srv/skills/alpha/SKILL.md"),
            "{detail}"
        );
    }

    #[test]
    fn drift_event_omits_skill_for_outside_source() {
        let source = Path::new("/srv/skills");
        let drift = DriftEvent::classify(
            source,
            Path::new("/tmp/random.txt"),
            DriftChangeKind::Created,
        );
        let event = drift.to_skill_event();
        assert_eq!(event.kind, SkillEventKind::SourceChanged);
        assert_eq!(event.skill_name, None);
        assert_eq!(event.relative_path, None);
        let detail = event.detail.as_deref().expect("detail must be set");
        assert!(detail.contains("scope=outside_source"), "{detail}");
        assert!(detail.contains("change=created"), "{detail}");
        assert!(detail.contains("path=/tmp/random.txt"), "{detail}");
    }

    #[test]
    fn drift_event_omits_relative_path_for_skill_dir() {
        let source = Path::new("/srv/skills");
        let drift = DriftEvent::classify(
            source,
            Path::new("/srv/skills/alpha"),
            DriftChangeKind::Deleted,
        );
        let event = drift.to_skill_event();
        assert_eq!(event.skill_name.as_deref(), Some("alpha"));
        assert_eq!(event.relative_path, None);
    }

    #[test]
    fn drift_event_with_scope_preserves_explicit_inputs() {
        let drift = DriftEvent::with_scope(
            DriftChangeKind::Renamed,
            DriftScope::InsideSkill {
                skill_name: "alpha".to_string(),
                relative_path: PathBuf::from("scripts/run.sh"),
            },
            "/srv/skills/alpha/scripts/run.sh",
        );
        let event = drift.to_skill_event();
        assert_eq!(event.skill_name.as_deref(), Some("alpha"));
        assert_eq!(
            event.relative_path.as_deref(),
            Some(Path::new("scripts/run.sh"))
        );
        let detail = event.detail.as_deref().unwrap();
        assert!(detail.contains("change=renamed"), "{detail}");
        assert!(detail.contains("scope=inside_skill"), "{detail}");
    }

    #[test]
    fn observer_emits_through_sink() {
        let source = Path::new("/srv/skills");
        let sink = Arc::new(InMemoryEventSink::new());
        let observer = SourceDriftObserver::new(source, sink.clone());
        let drift = observer.observe(
            Path::new("/srv/skills/alpha/SKILL.md"),
            DriftChangeKind::Modified,
        );
        assert!(matches!(drift.scope, DriftScope::SkillMd { .. }));
        let recorded = sink.events();
        assert_eq!(recorded.len(), 1);
        let event = &recorded[0];
        assert_eq!(event.kind, SkillEventKind::SourceChanged);
        assert_eq!(event.skill_name.as_deref(), Some("alpha"));
        assert_eq!(event.relative_path.as_deref(), Some(Path::new("SKILL.md")));
    }

    #[test]
    fn observer_default_sink_is_no_op() {
        let observer = SourceDriftObserver::no_op(Path::new("/srv/skills"));
        // Calling observe() on a no-op observer must not panic and must
        // not produce a side-effect we can detect from outside.
        let _ = observer.observe(
            Path::new("/srv/skills/alpha/SKILL.md"),
            DriftChangeKind::Modified,
        );
        assert_eq!(observer.source_root(), Path::new("/srv/skills"));
    }

    #[test]
    fn observer_observe_event_emits_without_reclassifying() {
        let sink = Arc::new(InMemoryEventSink::new());
        let observer = SourceDriftObserver::new(Path::new("/srv/skills"), sink.clone());
        let drift = DriftEvent::with_scope(
            DriftChangeKind::Created,
            DriftScope::SkillDir {
                skill_name: "beta".to_string(),
            },
            "/elsewhere/beta",
        );
        observer.observe_event(&drift);
        let recorded = sink.events();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].skill_name.as_deref(), Some("beta"));
        let detail = recorded[0].detail.as_deref().unwrap();
        // Even though the original_path is outside the source root, the
        // explicit scope wins because we used `with_scope`.
        assert!(detail.contains("scope=skill_dir"), "{detail}");
        assert!(detail.contains("path=/elsewhere/beta"), "{detail}");
    }
}
