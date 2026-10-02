//! Backend-neutral package query contract and shared types.
//!
//! [`PackageQuery`] is object-safe and backend-agnostic: RPM and (future) apt
//! backends both fit it. This module only declares the contract and output
//! types; concrete backends live in sibling modules (e.g. [`crate::rpm_query`]).
//!
//! "Not installed" is a normal branch ([`Option::None`]), not an error: the
//! observe/repair/update consumers treat absence as expected control flow, so
//! [`PackageQueryError`] is reserved for genuinely anomalous conditions.

use std::cmp::Ordering;
use std::fmt;

use thiserror::Error;

/// Version triple isomorphic to both RPM EVR and dpkg version.
///
/// Both backends share the `[epoch:]version[-release]` shape, so one neutral
/// type plus [`fmt::Display`] carries either; `release` maps to dpkg's
/// `debian_revision` and is `None` for native packages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageVersion {
    /// Epoch; `None` for the equivalent "no epoch" spellings (`(none)`, empty, or `0`).
    pub epoch: Option<String>,
    /// Upstream version.
    pub version: String,
    /// Release / debian_revision; `None` for native packages with no release.
    pub release: Option<String>,
}

impl fmt::Display for PackageVersion {
    /// Renders `[epoch:]version[-release]` — the EVR form for RPM and the full
    /// version string for dpkg.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(epoch) = &self.epoch {
            write!(f, "{epoch}:")?;
        }
        write!(f, "{}", self.version)?;
        if let Some(release) = &self.release {
            write!(f, "-{release}")?;
        }
        Ok(())
    }
}

/// Compare two RPM versions by RPM's EVR ordering (epoch, then version, then
/// release), returning whether `a` sorts before/equal/after `b`.
///
/// This is **not** semver: RPM EVRs routinely use shapes semver rejects
/// (numeric epochs like `1:...`, two-segment versions like `2.3`, `.al8`
/// releases, `~`/`^` markers). A semver comparison silently treats those as
/// unordered, so callers that need to know whether a repo candidate genuinely
/// upgrades an installed package must use this instead. Epoch is compared
/// numerically (absent epoch is `0`); version and release each use
/// [`rpmvercmp`].
pub fn rpm_evr_cmp(a: &PackageVersion, b: &PackageVersion) -> Ordering {
    let epoch_a = a.epoch.as_deref().unwrap_or("0");
    let epoch_b = b.epoch.as_deref().unwrap_or("0");
    let epoch_cmp = match (epoch_a.parse::<i64>(), epoch_b.parse::<i64>()) {
        (Ok(x), Ok(y)) => x.cmp(&y),
        // A non-numeric epoch is malformed; fall back to segment comparison
        // rather than panicking so a weird rpmdb row still orders deterministically.
        _ => rpmvercmp(epoch_a, epoch_b),
    };
    if epoch_cmp != Ordering::Equal {
        return epoch_cmp;
    }
    let version_cmp = rpmvercmp(&a.version, &b.version);
    if version_cmp != Ordering::Equal {
        return version_cmp;
    }
    rpmvercmp(
        a.release.as_deref().unwrap_or(""),
        b.release.as_deref().unwrap_or(""),
    )
}

/// Compare a single RPM version or release segment using RPM's `rpmvercmp`
/// algorithm (a faithful port of `lib/rpmvercmp.c`).
///
/// The string is walked in alternating runs of digits and letters separated by
/// any other bytes; numeric runs compare numerically (leading zeros stripped,
/// then longer-wins), alphabetic runs compare lexically, a digit run outranks
/// an alphabetic run, and `~` sorts before everything (including end of string)
/// while `^` sorts after a shorter version but before a longer one. Kept here in
/// the platform layer so version logic lives next to the RPM backend.
pub fn rpmvercmp(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let a = a.as_bytes();
    let b = b.as_bytes();
    let (mut i, mut j) = (0usize, 0usize);

    let is_sep = |c: u8| !c.is_ascii_alphanumeric() && c != b'~' && c != b'^';

    loop {
        while i < a.len() && is_sep(a[i]) {
            i += 1;
        }
        while j < b.len() && is_sep(b[j]) {
            j += 1;
        }

        // `~` sorts before everything, even the empty string.
        let a_tilde = i < a.len() && a[i] == b'~';
        let b_tilde = j < b.len() && b[j] == b'~';
        if a_tilde || b_tilde {
            if !a_tilde {
                return Ordering::Greater;
            }
            if !b_tilde {
                return Ordering::Less;
            }
            i += 1;
            j += 1;
            continue;
        }

        // `^` sorts after a shorter version but before a longer one.
        let a_caret = i < a.len() && a[i] == b'^';
        let b_caret = j < b.len() && b[j] == b'^';
        if a_caret || b_caret {
            if i >= a.len() {
                return Ordering::Less;
            }
            if j >= b.len() {
                return Ordering::Greater;
            }
            if !a_caret {
                return Ordering::Greater;
            }
            if !b_caret {
                return Ordering::Less;
            }
            i += 1;
            j += 1;
            continue;
        }

        if i >= a.len() || j >= b.len() {
            break;
        }

        let (start_i, start_j) = (i, j);
        let isnum = a[i].is_ascii_digit();
        if isnum {
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
        } else {
            while i < a.len() && a[i].is_ascii_alphabetic() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_alphabetic() {
                j += 1;
            }
        }

        let seg_a = &a[start_i..i];
        let seg_b = &b[start_j..j];

        // `seg_a` is always non-empty (we entered on an alnum byte of its type);
        // an empty `seg_b` means the two runs are different types. A numeric run
        // outranks an alphabetic one.
        if seg_b.is_empty() {
            return if isnum {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }

        if isnum {
            let na = strip_leading_zeros(seg_a);
            let nb = strip_leading_zeros(seg_b);
            match na.len().cmp(&nb.len()) {
                Ordering::Equal => match na.cmp(nb) {
                    Ordering::Equal => {}
                    other => return other,
                },
                other => return other,
            }
        } else {
            match seg_a.cmp(seg_b) {
                Ordering::Equal => {}
                other => return other,
            }
        }
    }

    // Whichever string still has content sorts higher.
    match (i >= a.len(), j >= b.len()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        // Unreachable: the loop only breaks when at least one side is exhausted.
        (false, false) => Ordering::Equal,
    }
}

fn strip_leading_zeros(mut s: &[u8]) -> &[u8] {
    while !s.is_empty() && s[0] == b'0' {
        s = &s[1..];
    }
    s
}

/// A package's identity, version, and origin (shared by installed/available queries).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageInfo {
    /// Package name as reported by the backend (e.g. rpm `%{NAME}`).
    pub name: String,
    /// Resolved version triple.
    pub version: PackageVersion,
    /// Architecture (e.g. `x86_64`, `noarch`).
    pub arch: String,
    /// Source repo/origin; installed queries typically yield `None` (or a
    /// backend-specific marker like `@System`).
    pub origin: Option<String>,
}

/// Errors raised by [`PackageQuery`] backends.
#[derive(Debug, Error)]
pub enum PackageQueryError {
    /// Explicit repository could not be fetched, verified, or parsed.
    #[error(transparent)]
    Repository(#[from] crate::rpm_metadata::RpmMetadataError),
    /// The backend binary could not be found (spawn `NotFound`).
    #[error("command not found: {command}")]
    CommandMissing {
        /// Backend binary that could not be found.
        command: String,
    },
    /// The backend binary existed but could not be executed (`PermissionDenied`).
    #[error("permission denied running {command}")]
    PermissionDenied {
        /// Backend binary that could not be executed.
        command: String,
    },
    /// The command ran but reported a hard failure (non-zero exit that is not
    /// the backend's "not installed" signal).
    #[error("{command} failed (code {code:?}): {stderr}")]
    QueryFailed {
        /// Backend binary that exited with a failure.
        command: String,
        /// Exit code; `None` if the process was killed by a signal.
        code: Option<i32>,
        /// Captured standard error from the failed command.
        stderr: String,
    },
    /// Output could not be parsed (wrong field count), or the single-instance
    /// invariant was violated ([`PackageQuery::query_installed`] got multiple
    /// rows = same-name package with several installed versions). `detail`
    /// describes the shape so callers can decide how to handle the drift.
    #[error("unexpected {command} output: {detail}")]
    UnexpectedOutput {
        /// Backend binary whose output was unexpected.
        command: String,
        /// Description of the malformed or invariant-violating output shape.
        detail: String,
    },
}

/// Backend-neutral package query contract.
///
/// All methods take `&self` and return concrete types, so the trait is
/// object-safe and any backend can be held as `Box<dyn PackageQuery>`.
pub trait PackageQuery {
    /// Query an installed package; not installed returns `Ok(None)`.
    ///
    /// # Errors
    /// See [`PackageQueryError`] for the failure conditions; absence of the
    /// package is **not** an error.
    fn query_installed(&self, package: &str) -> Result<Option<PackageInfo>, PackageQueryError>;

    /// Whether the package is installed.
    ///
    /// Default implementation delegates to [`query_installed`](Self::query_installed)
    /// so backends need not repeat it.
    fn is_installed(&self, package: &str) -> Result<bool, PackageQueryError> {
        Ok(self.query_installed(package)?.is_some())
    }

    /// Query available candidates in repos; no candidates yields an empty `Vec`.
    ///
    /// # Errors
    /// See [`PackageQueryError`].
    fn query_available(&self, package: &str) -> Result<Vec<PackageInfo>, PackageQueryError>;

    /// Source repo of an *installed* package (e.g. `@System`, `anolisa-release`);
    /// `None` when it cannot be determined.
    ///
    /// [`query_installed`](Self::query_installed) cannot report this (its
    /// `rpm -q` path yields no reponame, hence [`PackageInfo::origin`] is always
    /// `None` there), so adopt/observe callers query the origin separately to
    /// populate `source_repo`. The default returns `None` so backends without
    /// origin support still satisfy the trait.
    ///
    /// # Errors
    /// See [`PackageQueryError`]; "no origin" is `Ok(None)`, not an error.
    fn installed_origin(&self, _package: &str) -> Result<Option<String>, PackageQueryError> {
        Ok(None)
    }

    /// Names of *installed* packages that provide `capability` (an RPM virtual
    /// provide such as `anolisa-component(<name>)`), de-duplicated by name.
    ///
    /// No provider yields an empty `Vec` (a normal branch, not an error). The
    /// default returns empty so backends without provides lookup still satisfy
    /// the trait. Callers treat ≥2 distinct names as an ambiguous match.
    ///
    /// # Errors
    /// See [`PackageQueryError`]; "nothing provides it" is `Ok(vec![])`.
    fn what_provides_installed(&self, _capability: &str) -> Result<Vec<String>, PackageQueryError> {
        Ok(Vec::new())
    }

    /// Names of *available* repository packages that provide `capability`,
    /// de-duplicated by name.
    ///
    /// This is the repository-side counterpart to
    /// [`what_provides_installed`](Self::what_provides_installed). It lets
    /// callers resolve a component capability before install/adopt without
    /// requiring the package to be installed yet.
    ///
    /// # Errors
    /// See [`PackageQueryError`]; "nothing provides it" is `Ok(vec![])`.
    fn what_provides_available(&self, _capability: &str) -> Result<Vec<String>, PackageQueryError> {
        Ok(Vec::new())
    }

    /// Provides capabilities declared by an installed package.
    ///
    /// Used when user input may be a backend-native package name: the caller can
    /// inspect the package's own metadata and recover the ANOLISA component
    /// identity only if it declares `anolisa-component(<name>)`.
    ///
    /// # Errors
    /// See [`PackageQueryError`]; a missing package is represented as an empty
    /// capability list by backends that can distinguish that branch.
    fn provided_capabilities_installed(
        &self,
        _package: &str,
    ) -> Result<Vec<String>, PackageQueryError> {
        Ok(Vec::new())
    }

    /// Provides capabilities declared by an available repository package.
    ///
    /// This mirrors
    /// [`provided_capabilities_installed`](Self::provided_capabilities_installed)
    /// for packages not yet present on the host.
    ///
    /// # Errors
    /// See [`PackageQueryError`]; no available package yields `Ok(vec![])`.
    fn provided_capabilities_available(
        &self,
        _package: &str,
    ) -> Result<Vec<String>, PackageQueryError> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ver(epoch: Option<&str>, version: &str, release: Option<&str>) -> PackageVersion {
        PackageVersion {
            epoch: epoch.map(str::to_string),
            version: version.to_string(),
            release: release.map(str::to_string),
        }
    }

    #[test]
    fn rpmvercmp_matches_rpm_reference_cases() {
        // Canonical cases from RPM's own rpmvercmp test suite.
        assert_eq!(rpmvercmp("1.0", "1.0"), Ordering::Equal);
        assert_eq!(rpmvercmp("1.0", "2.0"), Ordering::Less);
        assert_eq!(rpmvercmp("2.0", "1.0"), Ordering::Greater);
        // Numeric segments compare numerically, not lexically: 10 > 2.
        assert_eq!(rpmvercmp("2", "10"), Ordering::Less);
        assert_eq!(rpmvercmp("1.10", "1.9"), Ordering::Greater);
        // Leading zeros are stripped before the length/lex compare.
        assert_eq!(rpmvercmp("1.0010", "1.10"), Ordering::Equal);
        // A numeric run outranks an alphabetic run at the same position.
        assert_eq!(rpmvercmp("1.a", "1.1"), Ordering::Less);
        // `~` sorts before everything, including a shorter release.
        assert_eq!(rpmvercmp("1.0~rc1", "1.0"), Ordering::Less);
        assert_eq!(rpmvercmp("1.0~rc1", "1.0~rc2"), Ordering::Less);
        // `^` sorts after the base version.
        assert_eq!(rpmvercmp("1.0^", "1.0"), Ordering::Greater);
    }

    #[test]
    fn rpm_evr_cmp_orders_epoch_first() {
        // A higher epoch wins regardless of version — the classic reason plain
        // version comparison is wrong.
        let older = ver(None, "2.0", Some("1.al8"));
        let newer = ver(Some("1"), "1.0", Some("1.al8"));
        assert_eq!(rpm_evr_cmp(&older, &newer), Ordering::Less);
        // Absent epoch is treated as 0, so it equals an explicit "0".
        assert_eq!(
            rpm_evr_cmp(&ver(None, "1.0", None), &ver(Some("0"), "1.0", None)),
            Ordering::Equal
        );
    }

    #[test]
    fn rpm_evr_cmp_detects_non_semver_upgrade() {
        // Real EVRs that semver cannot parse must still order correctly.
        let installed = ver(None, "0.5", Some("1.al4"));
        let candidate = ver(None, "1.0.0", Some("1.al4"));
        assert_eq!(rpm_evr_cmp(&installed, &candidate), Ordering::Less);
        // Release differences break upgrade ties.
        assert_eq!(
            rpm_evr_cmp(
                &ver(None, "1.0.0", Some("1.al4")),
                &ver(None, "1.0.0", Some("2.al4"))
            ),
            Ordering::Less
        );
    }
}
