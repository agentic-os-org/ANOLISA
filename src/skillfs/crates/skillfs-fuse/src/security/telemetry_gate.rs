//! Dynamic telemetry gate shared by every SkillFS SLS writer.
//!
//! When the deployment-owned sentinel file exists, SkillFS must not append new
//! records to the SLS telemetry log (`/var/log/anolisa/sls/ops/skillfs.jsonl`).
//! The gate is re-evaluated on every write, so creating or removing the
//! sentinel takes effect immediately without restarting the process — the
//! startup-time state is never cached.
//!
//! Fail-closed contract: telemetry is allowed **only** when the sentinel path
//! resolves to `ENOENT` (nothing is there). A regular file, directory, valid
//! symlink, or dangling symlink each disable telemetry, and any non-`ENOENT`
//! stat error (e.g. `EACCES`) also disables it. [`std::fs::symlink_metadata`]
//! is used rather than [`std::path::Path::exists`] so a dangling symlink is
//! treated as "present" (disable) instead of "absent" (allow).

use std::path::Path;

/// Deployment-owned sentinel; its presence disables SLS telemetry writes.
pub const TELEMETRY_DISABLED_SENTINEL: &str = "/etc/anolisa/.telemetry_disabled";

/// Returns `true` when SLS telemetry writes are permitted, i.e. the sentinel at
/// [`TELEMETRY_DISABLED_SENTINEL`] does not exist. Convenience wrapper over
/// [`telemetry_allowed_at`] pinned to the production sentinel; re-checks the
/// filesystem on every call — disabled is a normal, silent state, so callers
/// skip without emitting a warning.
pub fn telemetry_allowed() -> bool {
    telemetry_allowed_at(Path::new(TELEMETRY_DISABLED_SENTINEL))
}

/// Path-injectable form of the gate. Production writers pin the argument to
/// [`TELEMETRY_DISABLED_SENTINEL`] (via their default sentinel or
/// [`telemetry_allowed`]) — there is no env var or config that repoints the
/// production gate. This form is exposed so the writers' unit tests can point
/// the gate at a controlled temp path instead of depending on the host's real
/// `/etc/anolisa/.telemetry_disabled`.
///
/// Telemetry is allowed only when `symlink_metadata` reports `ENOENT`. Every
/// other outcome — the path exists in any form (file, dir, valid or dangling
/// symlink), or the stat fails for any other reason — fails closed.
pub fn telemetry_allowed_at(path: &Path) -> bool {
    matches!(
        std::fs::symlink_metadata(path),
        Err(error) if error.raw_os_error() == Some(libc::ENOENT)
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convenience_wrapper_pins_production_sentinel() {
        // Host-independent: whatever the real sentinel state is, the no-arg
        // wrapper must agree with the pinned-path form.
        assert_eq!(
            telemetry_allowed(),
            telemetry_allowed_at(Path::new(TELEMETRY_DISABLED_SENTINEL))
        );
    }

    #[test]
    fn allows_when_sentinel_absent() {
        let dir = tempfile::tempdir().unwrap();
        let sentinel = dir.path().join(".telemetry_disabled");
        assert!(
            telemetry_allowed_at(&sentinel),
            "missing sentinel must allow telemetry"
        );
    }

    #[test]
    fn disables_immediately_after_sentinel_created() {
        let dir = tempfile::tempdir().unwrap();
        let sentinel = dir.path().join(".telemetry_disabled");

        assert!(telemetry_allowed_at(&sentinel));
        std::fs::File::create(&sentinel).unwrap();
        assert!(
            !telemetry_allowed_at(&sentinel),
            "creating the sentinel must disable telemetry on the next check"
        );
    }

    #[test]
    fn restores_immediately_after_sentinel_removed() {
        let dir = tempfile::tempdir().unwrap();
        let sentinel = dir.path().join(".telemetry_disabled");

        std::fs::File::create(&sentinel).unwrap();
        assert!(!telemetry_allowed_at(&sentinel));
        std::fs::remove_file(&sentinel).unwrap();
        assert!(
            telemetry_allowed_at(&sentinel),
            "removing the sentinel must restore telemetry on the next check"
        );
    }

    #[test]
    fn directory_sentinel_disables() {
        let dir = tempfile::tempdir().unwrap();
        let sentinel = dir.path().join(".telemetry_disabled");
        std::fs::create_dir(&sentinel).unwrap();
        assert!(
            !telemetry_allowed_at(&sentinel),
            "a directory at the sentinel path must disable telemetry"
        );
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_disables() {
        let dir = tempfile::tempdir().unwrap();
        let sentinel = dir.path().join(".telemetry_disabled");
        // Target intentionally does not exist: symlink_metadata stats the link
        // itself (not the target), so this must fail closed and disable.
        std::os::unix::fs::symlink(dir.path().join("nonexistent-target"), &sentinel).unwrap();
        assert!(
            !telemetry_allowed_at(&sentinel),
            "a dangling symlink must disable telemetry, unlike Path::exists()"
        );
    }
}
