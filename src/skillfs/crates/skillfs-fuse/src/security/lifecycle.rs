//! Skill Security lifecycle namespace reservation (Packages S3 + S3.1).
//!
//! Future Skill Security packages will use a small set of reserved top-level
//! directory names — `.staging`, `.certified`, `.quarantine`, `.archive` — to
//! drive lifecycle transitions (staging → certified → active →
//! quarantined / archived). Package S3 reserved the names so the boundary
//! cannot be re-used for ordinary skills.
//!
//! Package S3.1 layers a *pure* management-view contract on top of that
//! reservation: [`LifecycleViewMode`] distinguishes the ordinary
//! agent-facing view (where reserved roots stay hidden and immutable) from
//! a future management view (where the same roots are intentionally exposed
//! to a trusted writer / management tool). The helpers in this module
//! centralize that decision. S3.1 ships only the contract — no FUSE
//! callback consults [`LifecycleViewMode::Management`] today and no CLI
//! flag turns it on. Default behavior is exactly the S3 behavior: ordinary
//! mounts hide and deny reserved roots.
//!
//! Helpers in this module are pure-lexical (no syscalls, no follow). They
//! centralize the "is this a reserved lifecycle namespace name?" and "may
//! this view see / mutate the name?" decisions so FUSE callbacks and
//! future policy implementations agree on the answer.

/// `.staging`: pre-certification holding area for newly imported skills.
pub const LIFECYCLE_STAGING: &str = ".staging";

/// `.certified`: skills that passed certification but have not been
/// activated for ordinary view exposure yet.
pub const LIFECYCLE_CERTIFIED: &str = ".certified";

/// `.quarantine`: skills that policy has flagged and removed from active
/// use without deleting the underlying source.
pub const LIFECYCLE_QUARANTINE: &str = ".quarantine";

/// `.archive`: skills retired from active use, kept for audit history.
pub const LIFECYCLE_ARCHIVE: &str = ".archive";

/// All reserved lifecycle namespace names. The order matches the staging
/// → certified → active → quarantine / archive flow but the reservation
/// itself is order-independent.
pub const LIFECYCLE_RESERVED_NAMES: &[&str] = &[
    LIFECYCLE_STAGING,
    LIFECYCLE_CERTIFIED,
    LIFECYCLE_QUARANTINE,
    LIFECYCLE_ARCHIVE,
];

/// Classification of a candidate top-level skill-name component.
///
/// `parse_path` exposes the top-level segment (after `/skills/` in normal
/// mode or after `/` in in-place mode) as `skill_name` on
/// `PathType::SkillDir`, `PathType::SkillMd`, and `PathType::Passthrough`.
/// `classify_skill_name` runs against that string and returns a `Reserved`
/// variant when the segment matches a lifecycle namespace name exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleNameClass {
    /// Not a reserved lifecycle name. Ordinary skill-name handling applies.
    Ordinary,
    /// Exact match for a reserved lifecycle namespace name.
    Reserved(&'static str),
}

/// Returns `true` when `name` exactly matches one of the reserved lifecycle
/// namespace names (`.staging`, `.certified`, `.quarantine`, `.archive`).
///
/// The match is case-sensitive and exact. Neighbours such as `.staging2`,
/// `.staging.bak`, `staging`, and ` .staging` are not reserved.
pub fn is_reserved_lifecycle_name(name: &str) -> bool {
    LIFECYCLE_RESERVED_NAMES
        .iter()
        .any(|reserved| *reserved == name)
}

/// Classify a candidate top-level skill-name segment.
///
/// Returns `LifecycleNameClass::Reserved(<canonical>)` when `skill_name`
/// matches a reserved lifecycle namespace name exactly; otherwise
/// `LifecycleNameClass::Ordinary`. The returned `&'static str` references
/// the canonical constant from this module so callers can include it in
/// audit detail without copying.
pub fn classify_skill_name(skill_name: &str) -> LifecycleNameClass {
    match skill_name {
        LIFECYCLE_STAGING => LifecycleNameClass::Reserved(LIFECYCLE_STAGING),
        LIFECYCLE_CERTIFIED => LifecycleNameClass::Reserved(LIFECYCLE_CERTIFIED),
        LIFECYCLE_QUARANTINE => LifecycleNameClass::Reserved(LIFECYCLE_QUARANTINE),
        LIFECYCLE_ARCHIVE => LifecycleNameClass::Reserved(LIFECYCLE_ARCHIVE),
        _ => LifecycleNameClass::Ordinary,
    }
}

/// Caller-facing view of lifecycle reservations (Package S3.1 contract).
///
/// SkillFS currently exposes only the ordinary view through the FUSE mount.
/// A future Skill Security package will add a management view that lets a
/// trusted writer (e.g. `skill-ledger`) intentionally observe and mutate
/// the reserved roots. S3.1 defines the boundary; no FUSE callback or CLI
/// flag selects [`Management`] yet.
///
/// The default is [`Ordinary`], which matches S3 behavior: reserved
/// lifecycle namespaces are hidden from ordinary `lookup`/`readdir` and
/// mutations targeting them are denied before physical I/O.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LifecycleViewMode {
    /// Agent-facing view: reserved lifecycle namespaces are hidden from
    /// `lookup`/`readdir` and immutable. This is the only mode wired into
    /// the default FUSE mount today.
    #[default]
    Ordinary,
    /// Trusted management view: reserved lifecycle namespaces are
    /// intentionally visible and mutable so a future management tool can
    /// drive lifecycle transitions. S3.1 only describes the contract —
    /// nothing in the FUSE runtime selects this variant yet.
    Management,
}

impl LifecycleViewMode {
    /// `true` when the caller is using the management view.
    pub const fn is_management(self) -> bool {
        matches!(self, Self::Management)
    }

    /// `true` when the caller is using the ordinary agent-facing view.
    pub const fn is_ordinary(self) -> bool {
        matches!(self, Self::Ordinary)
    }
}

/// Decide whether `name` should appear in `lookup`/`readdir` for a caller
/// using `mode`.
///
/// Non-reserved names always remain visible. Reserved lifecycle names are
/// hidden in [`LifecycleViewMode::Ordinary`] and exposed in
/// [`LifecycleViewMode::Management`].
///
/// This helper is pure-lexical and does not touch the filesystem. S3.1
/// callers consult it instead of open-coding the reservation table.
pub fn is_lifecycle_name_visible(name: &str, mode: LifecycleViewMode) -> bool {
    if is_reserved_lifecycle_name(name) {
        mode.is_management()
    } else {
        true
    }
}

/// Decide whether mutations targeting `name` should be allowed for a
/// caller using `mode`.
///
/// Mutation here covers the operations the S3 reservation already gates:
/// `mkdir`, `create`, write-open, `write`, `unlink`, `rmdir`, `rename`
/// from/to, and mutating `setattr`. Non-reserved names always remain
/// mutable. Reserved lifecycle names are denied in
/// [`LifecycleViewMode::Ordinary`] and permitted in
/// [`LifecycleViewMode::Management`].
///
/// This helper is the management-mode complement to the existing
/// `enforce_lifecycle_reservation` gate in `lib.rs`. The gate continues to
/// hard-code [`LifecycleViewMode::Ordinary`] semantics; S3.1 only defines
/// the contract a future trusted-writer surface would consult.
pub fn is_lifecycle_name_mutable(name: &str, mode: LifecycleViewMode) -> bool {
    if is_reserved_lifecycle_name(name) {
        mode.is_management()
    } else {
        true
    }
}

/// Per-mode access classification for a candidate top-level skill-name
/// segment.
///
/// `Ordinary` matches a non-reserved name and behaves like
/// [`LifecycleNameClass::Ordinary`]. `Hidden` is a reserved name observed
/// from the agent-facing view — both invisible and immutable.
/// `Exposed` is a reserved name observed from the management view, where a
/// future trusted writer is meant to see and mutate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleAccess {
    /// Not a reserved lifecycle name. Ordinary skill-name handling applies
    /// regardless of `mode`.
    Ordinary,
    /// Reserved lifecycle name observed from
    /// [`LifecycleViewMode::Ordinary`]. The name must stay hidden from
    /// `lookup`/`readdir` and mutations must be denied with `EACCES`.
    Hidden(&'static str),
    /// Reserved lifecycle name observed from
    /// [`LifecycleViewMode::Management`]. The name is intentionally
    /// visible and mutable. No FUSE callback selects this variant in
    /// S3.1; it exists so future wiring has a single, named outcome to
    /// match on.
    Exposed(&'static str),
}

impl LifecycleAccess {
    /// `true` when the access decision should treat the name as visible
    /// in `lookup`/`readdir`.
    pub const fn is_visible(self) -> bool {
        match self {
            LifecycleAccess::Ordinary => true,
            LifecycleAccess::Hidden(_) => false,
            LifecycleAccess::Exposed(_) => true,
        }
    }

    /// `true` when the access decision should allow mutations to proceed.
    pub const fn is_mutable(self) -> bool {
        match self {
            LifecycleAccess::Ordinary => true,
            LifecycleAccess::Hidden(_) => false,
            LifecycleAccess::Exposed(_) => true,
        }
    }

    /// The canonical reserved name when this access decision concerns a
    /// reserved lifecycle namespace, otherwise `None`.
    pub const fn reserved_name(self) -> Option<&'static str> {
        match self {
            LifecycleAccess::Ordinary => None,
            LifecycleAccess::Hidden(name) | LifecycleAccess::Exposed(name) => Some(name),
        }
    }
}

/// Classify a candidate top-level skill-name segment under a specific
/// caller view.
///
/// Returns [`LifecycleAccess::Ordinary`] for non-reserved names regardless
/// of `mode`. For reserved names, returns [`LifecycleAccess::Hidden`] in
/// [`LifecycleViewMode::Ordinary`] and [`LifecycleAccess::Exposed`] in
/// [`LifecycleViewMode::Management`]. The carried `&'static str`
/// references the canonical constant from this module so audit detail can
/// avoid an allocation.
pub fn classify_skill_name_with_mode(name: &str, mode: LifecycleViewMode) -> LifecycleAccess {
    match classify_skill_name(name) {
        LifecycleNameClass::Ordinary => LifecycleAccess::Ordinary,
        LifecycleNameClass::Reserved(canonical) => match mode {
            LifecycleViewMode::Ordinary => LifecycleAccess::Hidden(canonical),
            LifecycleViewMode::Management => LifecycleAccess::Exposed(canonical),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_names_set_is_exactly_four() {
        assert_eq!(LIFECYCLE_RESERVED_NAMES.len(), 4);
        assert!(LIFECYCLE_RESERVED_NAMES.contains(&LIFECYCLE_STAGING));
        assert!(LIFECYCLE_RESERVED_NAMES.contains(&LIFECYCLE_CERTIFIED));
        assert!(LIFECYCLE_RESERVED_NAMES.contains(&LIFECYCLE_QUARANTINE));
        assert!(LIFECYCLE_RESERVED_NAMES.contains(&LIFECYCLE_ARCHIVE));
    }

    #[test]
    #[allow(clippy::explicit_auto_deref)]
    fn matches_each_reserved_name() {
        for reserved in LIFECYCLE_RESERVED_NAMES {
            assert!(
                is_reserved_lifecycle_name(reserved),
                "{} should be reserved",
                reserved
            );
            assert_eq!(
                classify_skill_name(reserved),
                LifecycleNameClass::Reserved(*reserved)
            );
        }
    }

    #[test]
    fn rejects_neighbour_names() {
        for neighbour in [
            ".staging2",
            ".staging.bak",
            ".staging-1",
            "staging",
            ".certifi",
            ".certified2",
            ".quarantin",
            ".quarantines",
            ".archive2",
            ".archives",
            "archive",
            ".",
            ".. ",
            "",
            "alpha",
            "skill-discover",
        ] {
            assert!(
                !is_reserved_lifecycle_name(neighbour),
                "{:?} must not be reserved",
                neighbour
            );
            assert_eq!(
                classify_skill_name(neighbour),
                LifecycleNameClass::Ordinary,
                "{:?} must classify as Ordinary",
                neighbour
            );
        }
    }

    #[test]
    fn match_is_case_sensitive() {
        // Linux filesystems are case-sensitive, and lifecycle names are
        // ASCII. Different casings must not collide with the reservation.
        for variant in [
            ".STAGING",
            ".Staging",
            ".CERTIFIED",
            ".QuArAnTiNe",
            ".Archive",
        ] {
            assert!(
                !is_reserved_lifecycle_name(variant),
                "{:?} must not be reserved (case sensitivity)",
                variant
            );
        }
    }

    #[test]
    fn match_does_not_apply_to_nested_segments() {
        // Reservation only covers the top-level skill-name segment. Nested
        // paths such as `docs/.staging` are handled by callers passing the
        // skill_name component, not a multi-segment string.
        assert!(!is_reserved_lifecycle_name("docs/.staging"));
        assert!(!is_reserved_lifecycle_name(".staging/extra"));
        assert!(!is_reserved_lifecycle_name("alpha/.staging"));
    }

    #[test]
    fn classify_returns_canonical_static_string() {
        // The Reserved variant carries the canonical lifecycle value for
        // audit details rather than the caller-provided spelling.
        match classify_skill_name(".staging") {
            LifecycleNameClass::Reserved(name) => {
                assert_eq!(name, LIFECYCLE_STAGING);
            }
            LifecycleNameClass::Ordinary => panic!(".staging must be reserved"),
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Package S3.1 — management-view contract tests.
    //
    // These prove the pure API: defaults match S3 behavior (reserved
    // roots stay hidden + immutable), management mode classification can
    // intentionally expose the same roots, and neighbour / non-reserved
    // names behave the same in both modes. The default FUSE mount does
    // not select `Management` and these helpers are not yet consumed by
    // `lib.rs`; that wiring is deliberately out of S3.1 scope.
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn lifecycle_view_mode_default_is_ordinary() {
        assert_eq!(LifecycleViewMode::default(), LifecycleViewMode::Ordinary);
        assert!(LifecycleViewMode::default().is_ordinary());
        assert!(!LifecycleViewMode::default().is_management());
    }

    #[test]
    fn lifecycle_view_mode_predicates_are_mutually_exclusive() {
        assert!(LifecycleViewMode::Ordinary.is_ordinary());
        assert!(!LifecycleViewMode::Ordinary.is_management());
        assert!(LifecycleViewMode::Management.is_management());
        assert!(!LifecycleViewMode::Management.is_ordinary());
    }

    #[test]
    fn ordinary_mode_hides_every_reserved_root() {
        for reserved in LIFECYCLE_RESERVED_NAMES {
            assert!(
                !is_lifecycle_name_visible(reserved, LifecycleViewMode::Ordinary),
                "{} must be hidden in Ordinary mode",
                reserved
            );
            assert!(
                !is_lifecycle_name_mutable(reserved, LifecycleViewMode::Ordinary),
                "{} must be immutable in Ordinary mode",
                reserved
            );
        }
    }

    #[test]
    fn management_mode_exposes_every_reserved_root() {
        for reserved in LIFECYCLE_RESERVED_NAMES {
            assert!(
                is_lifecycle_name_visible(reserved, LifecycleViewMode::Management),
                "{} must be visible in Management mode",
                reserved
            );
            assert!(
                is_lifecycle_name_mutable(reserved, LifecycleViewMode::Management),
                "{} must be mutable in Management mode",
                reserved
            );
        }
    }

    #[test]
    fn non_reserved_names_are_visible_and_mutable_in_every_mode() {
        // Names that are not on the reservation list are unaffected by the
        // mode. The S3 neighbour cases stay neighbours under S3.1.
        for ordinary in [
            "alpha",
            "skill-discover",
            ".staging2",
            ".staging.bak",
            "staging",
            ".archives",
            "regular-skill",
        ] {
            for mode in [LifecycleViewMode::Ordinary, LifecycleViewMode::Management] {
                assert!(
                    is_lifecycle_name_visible(ordinary, mode),
                    "{:?} must be visible under {:?}",
                    ordinary,
                    mode
                );
                assert!(
                    is_lifecycle_name_mutable(ordinary, mode),
                    "{:?} must be mutable under {:?}",
                    ordinary,
                    mode
                );
                assert_eq!(
                    classify_skill_name_with_mode(ordinary, mode),
                    LifecycleAccess::Ordinary,
                    "{:?} must classify as Ordinary under {:?}",
                    ordinary,
                    mode
                );
            }
        }
    }

    #[test]
    fn classify_with_mode_returns_hidden_in_ordinary() {
        for reserved in LIFECYCLE_RESERVED_NAMES {
            match classify_skill_name_with_mode(reserved, LifecycleViewMode::Ordinary) {
                LifecycleAccess::Hidden(name) => {
                    assert_eq!(name, *reserved);
                }
                other => panic!(
                    "{} under Ordinary expected Hidden, got {:?}",
                    reserved, other
                ),
            }
        }
        match classify_skill_name_with_mode(LIFECYCLE_STAGING, LifecycleViewMode::Ordinary) {
            LifecycleAccess::Hidden(name) => assert_eq!(name, LIFECYCLE_STAGING),
            other => panic!("expected Hidden, got {:?}", other),
        }
    }

    #[test]
    fn classify_with_mode_returns_exposed_in_management() {
        for reserved in LIFECYCLE_RESERVED_NAMES {
            match classify_skill_name_with_mode(reserved, LifecycleViewMode::Management) {
                LifecycleAccess::Exposed(name) => {
                    assert_eq!(name, *reserved);
                }
                other => panic!(
                    "{} under Management expected Exposed, got {:?}",
                    reserved, other
                ),
            }
        }
        match classify_skill_name_with_mode(LIFECYCLE_STAGING, LifecycleViewMode::Management) {
            LifecycleAccess::Exposed(name) => assert_eq!(name, LIFECYCLE_STAGING),
            other => panic!("expected Exposed, got {:?}", other),
        }
    }

    #[test]
    fn lifecycle_access_visibility_and_mutability_predicates_match_helpers() {
        // The `LifecycleAccess::is_visible` / `is_mutable` predicates must
        // agree with the standalone `is_lifecycle_name_visible` /
        // `is_lifecycle_name_mutable` helpers for every name × mode pair.
        let cases = [
            "alpha",
            "skill-discover",
            ".staging",
            ".certified",
            ".quarantine",
            ".archive",
            ".staging2",
            "staging",
        ];
        for name in cases {
            for mode in [LifecycleViewMode::Ordinary, LifecycleViewMode::Management] {
                let access = classify_skill_name_with_mode(name, mode);
                assert_eq!(
                    access.is_visible(),
                    is_lifecycle_name_visible(name, mode),
                    "visibility mismatch for {:?} under {:?}",
                    name,
                    mode
                );
                assert_eq!(
                    access.is_mutable(),
                    is_lifecycle_name_mutable(name, mode),
                    "mutability mismatch for {:?} under {:?}",
                    name,
                    mode
                );
            }
        }
    }

    #[test]
    fn lifecycle_access_reserved_name_round_trips_canonical_constant() {
        // Reserved name surfaces the canonical &'static str regardless of
        // mode; Ordinary access reports None.
        assert_eq!(LifecycleAccess::Ordinary.reserved_name(), None);
        for reserved in LIFECYCLE_RESERVED_NAMES {
            assert_eq!(
                classify_skill_name_with_mode(reserved, LifecycleViewMode::Ordinary)
                    .reserved_name(),
                Some(*reserved)
            );
            assert_eq!(
                classify_skill_name_with_mode(reserved, LifecycleViewMode::Management)
                    .reserved_name(),
                Some(*reserved)
            );
        }
    }

    #[test]
    fn management_mode_does_not_relax_invalid_skill_name_neighbours() {
        // Names that look adjacent to a reserved root but do not match
        // exactly stay non-reserved under both modes. Management mode is
        // a per-mode *view* of the reservation table — it does not add new
        // reservations or pull in case-variant matches.
        for variant in [
            ".STAGING",
            ".Staging",
            ".CERTIFIED",
            ".QuArAnTiNe",
            ".Archive",
            " .staging",
            ".staging ",
        ] {
            for mode in [LifecycleViewMode::Ordinary, LifecycleViewMode::Management] {
                assert!(is_lifecycle_name_visible(variant, mode));
                assert!(is_lifecycle_name_mutable(variant, mode));
                assert_eq!(
                    classify_skill_name_with_mode(variant, mode),
                    LifecycleAccess::Ordinary
                );
            }
        }
    }
}
