//! Version-pinned RPM candidate selection.
//!
//! Resolves what a `--version` pin means against the candidates a repository
//! query returned: it matches the RPM VERSION field (not the whole EVR), keeps
//! only builds this host can run (`host_arch` or `noarch`), and breaks ties by
//! highest EVR via [`rpm_evr_cmp`]. The winner renders to an exact NEVRA for
//! the native transaction, while the caller keeps the bare package name for
//! rpmdb observation and persisted state — the pin never leaks into identity.

use crate::pkg_query::{PackageInfo, rpm_evr_cmp};

/// Outcome of resolving a `--version` pin against repository candidates.
///
/// The two failure branches are distinguished so the caller can render an
/// actionable message: an absent version is a bad `--version`, while an
/// arch-only miss names both the version and the host architecture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinnedSelection {
    /// A host-compatible candidate matched the requested version.
    Selected(PackageInfo),
    /// No candidate carried the requested VERSION for any architecture.
    VersionAbsent,
    /// The version exists, but only for architectures this host cannot run.
    /// Carries the offered arches (sorted, de-duplicated) for the message.
    ArchUnsupported {
        /// Architectures the requested version is published for.
        offered: Vec<String>,
    },
}

/// Select the version-pinned candidate for `host_arch`.
///
/// `requested_version` is matched against [`PackageInfo`]'s `version.version`
/// (the RPM VERSION field) exactly — a `--version 0.6.2` never widens to an
/// EVR such as `0.6.2-2`. Among candidates that both match the version and run
/// on the host (`host_arch` or `noarch`), the highest EVR wins deterministically
/// via [`rpm_evr_cmp`]; an EVR tie prefers the exact host arch, then orders by
/// arch name so the choice is stable. The version match is never relaxed.
pub fn select_pinned_candidate(
    candidates: &[PackageInfo],
    requested_version: &str,
    host_arch: &str,
) -> PinnedSelection {
    let best = candidates
        .iter()
        .filter(|c| c.version.version == requested_version)
        .filter(|c| c.arch == host_arch || c.arch == "noarch")
        .max_by(|a, b| {
            rpm_evr_cmp(&a.version, &b.version)
                // On equal EVR, prefer the exact host arch over noarch, then
                // fall back to arch-name order for a deterministic result.
                .then_with(|| host_rank(a, host_arch).cmp(&host_rank(b, host_arch)))
                .then_with(|| a.arch.cmp(&b.arch))
        });

    match best {
        Some(info) => PinnedSelection::Selected(info.clone()),
        None => {
            let mut offered: Vec<String> = candidates
                .iter()
                .filter(|c| c.version.version == requested_version)
                .map(|c| c.arch.clone())
                .collect();
            if offered.is_empty() {
                return PinnedSelection::VersionAbsent;
            }
            offered.sort();
            offered.dedup();
            PinnedSelection::ArchUnsupported { offered }
        }
    }
}

/// Rank giving the exact host arch precedence over `noarch` on an EVR tie.
fn host_rank(info: &PackageInfo, host_arch: &str) -> u8 {
    u8::from(info.arch == host_arch)
}

/// Render `info` to the exact NEVRA a native transaction accepts as a pinned
/// target: `name-[epoch:]version-release.arch` (the epoch and release are
/// emitted by [`PackageInfo`]'s version [`Display`], so an epoch-bearing
/// candidate yields `name-epoch:version-release.arch`).
///
/// [`Display`]: std::fmt::Display
pub fn nevra(info: &PackageInfo) -> String {
    format!("{}-{}.{}", info.name, info.version, info.arch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkg_query::PackageVersion;

    fn candidate(version: &str, release: &str, arch: &str) -> PackageInfo {
        candidate_epoch(None, version, release, arch)
    }

    fn candidate_epoch(
        epoch: Option<&str>,
        version: &str,
        release: &str,
        arch: &str,
    ) -> PackageInfo {
        PackageInfo {
            name: "agentsight".to_string(),
            version: PackageVersion {
                epoch: epoch.map(str::to_string),
                version: version.to_string(),
                release: Some(release.to_string()),
            },
            arch: arch.to_string(),
            origin: Some("anolisa-configured".to_string()),
        }
    }

    #[test]
    fn pins_requested_version_over_a_newer_build() {
        // The repo offers 0.6.2 and a newer 0.7.0; a 0.6.2 pin resolves to
        // 0.6.2, never the newer candidate.
        let candidates = vec![
            candidate("0.6.2", "1.alnx4", "x86_64"),
            candidate("0.7.0", "1.alnx4", "x86_64"),
        ];
        let selection = select_pinned_candidate(&candidates, "0.6.2", "x86_64");
        let PinnedSelection::Selected(info) = selection else {
            panic!("expected a selected candidate, got {selection:?}");
        };
        assert_eq!(info.version.version, "0.6.2");
        assert_eq!(nevra(&info), "agentsight-0.6.2-1.alnx4.x86_64");
    }

    #[test]
    fn matches_version_field_not_the_full_evr() {
        // "0.6.2" must not match a candidate whose VERSION is "0.6.20".
        let candidates = vec![candidate("0.6.20", "1.alnx4", "x86_64")];
        assert_eq!(
            select_pinned_candidate(&candidates, "0.6.2", "x86_64"),
            PinnedSelection::VersionAbsent
        );
    }

    #[test]
    fn picks_highest_release_for_one_version() {
        // Several releases of the same version: the highest EVR wins by
        // rpm_evr_cmp (release 10 outranks release 2, which is numeric).
        let candidates = vec![
            candidate("0.6.2", "2.alnx4", "x86_64"),
            candidate("0.6.2", "10.alnx4", "x86_64"),
            candidate("0.6.2", "1.alnx4", "x86_64"),
        ];
        let PinnedSelection::Selected(info) =
            select_pinned_candidate(&candidates, "0.6.2", "x86_64")
        else {
            panic!("expected a selected candidate");
        };
        assert_eq!(nevra(&info), "agentsight-0.6.2-10.alnx4.x86_64");
    }

    #[test]
    fn higher_epoch_outranks_higher_release() {
        // A non-zero epoch dominates the EVR ordering regardless of release.
        let candidates = vec![
            candidate("0.6.2", "9.alnx4", "x86_64"),
            candidate_epoch(Some("1"), "0.6.2", "1.alnx4", "x86_64"),
        ];
        let PinnedSelection::Selected(info) =
            select_pinned_candidate(&candidates, "0.6.2", "x86_64")
        else {
            panic!("expected a selected candidate");
        };
        assert_eq!(info.version.epoch.as_deref(), Some("1"));
        assert_eq!(nevra(&info), "agentsight-1:0.6.2-1.alnx4.x86_64");
    }

    #[test]
    fn keeps_host_arch_and_noarch_but_excludes_others() {
        // aarch64 host: the aarch64 and noarch builds are eligible, the
        // x86_64 build is excluded even though it carries the same version.
        let candidates = vec![
            candidate("0.6.2", "1.alnx4", "x86_64"),
            candidate("0.6.2", "1.alnx4", "aarch64"),
        ];
        let PinnedSelection::Selected(info) =
            select_pinned_candidate(&candidates, "0.6.2", "aarch64")
        else {
            panic!("expected a selected candidate");
        };
        assert_eq!(info.arch, "aarch64");

        let noarch = vec![candidate("0.6.2", "1.alnx4", "noarch")];
        let PinnedSelection::Selected(info) = select_pinned_candidate(&noarch, "0.6.2", "aarch64")
        else {
            panic!("expected the noarch candidate to be host-compatible");
        };
        assert_eq!(info.arch, "noarch");
    }

    #[test]
    fn version_present_only_for_foreign_arch_is_arch_unsupported() {
        // The version exists, but only for x86_64 — an aarch64 host cannot use
        // it, and the miss names the offered arch rather than falling back.
        let candidates = vec![
            candidate("0.6.2", "1.alnx4", "x86_64"),
            candidate("0.6.2", "2.alnx4", "x86_64"),
        ];
        assert_eq!(
            select_pinned_candidate(&candidates, "0.6.2", "aarch64"),
            PinnedSelection::ArchUnsupported {
                offered: vec!["x86_64".to_string()]
            }
        );
    }

    #[test]
    fn prefers_exact_host_arch_over_noarch_on_evr_tie() {
        // Same EVR published as both host-arch and noarch: the exact host arch
        // is chosen so the installed artifact is the native build.
        let candidates = vec![
            candidate("0.6.2", "1.alnx4", "noarch"),
            candidate("0.6.2", "1.alnx4", "aarch64"),
        ];
        let PinnedSelection::Selected(info) =
            select_pinned_candidate(&candidates, "0.6.2", "aarch64")
        else {
            panic!("expected a selected candidate");
        };
        assert_eq!(info.arch, "aarch64");
    }

    #[test]
    fn no_candidates_is_version_absent() {
        assert_eq!(
            select_pinned_candidate(&[], "0.6.2", "x86_64"),
            PinnedSelection::VersionAbsent
        );
    }
}
