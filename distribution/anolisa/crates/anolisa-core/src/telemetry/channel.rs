//! Telemetry collection channel: opt-out gating + idempotent setup.
//!
//! Collection is **on by default**. The channel owns the on/off decision via a
//! single opt-out marker: components collect unless `DISABLE_MARKER_PATH`
//! exists. This module owns the ops directory / `.jsonl` files / logrotate and
//! toggling that marker.
//!
//! Uploader lifecycle (lazy spawn) lives in [`crate::telemetry::uploader`];
//! the CLI wires "enable → enable_collection + ensure uploader running" so this
//! module stays pure filesystem and unit-testable without spawning processes.

use std::fs;
use std::path::{Path, PathBuf};

use crate::telemetry::{OpsLayout, TelemetryConfig, TelemetryError};

/// Opt-out marker gating default telemetry collection.
///
/// Absent → collection is on (the default); present → components skip and the
/// uploader stays idle. Written only by `anolisa telemetry disable` (and
/// `unregister`); removed by `enable` / `register`.
pub const DISABLE_MARKER_PATH: &str = "/etc/anolisa/.telemetry_disabled";

/// Owns the opt-out marker and the ops channel filesystem layout.
pub struct TelemetryChannel {
    config: TelemetryConfig,
    disable_marker_path: PathBuf,
}

impl Default for TelemetryChannel {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetryChannel {
    /// Construct with production paths.
    pub fn new() -> Self {
        Self {
            config: TelemetryConfig::default(),
            disable_marker_path: PathBuf::from(DISABLE_MARKER_PATH),
        }
    }

    /// Construct with injected config + opt-out marker path (unit tests only).
    pub fn with_paths(config: TelemetryConfig, disable_marker_path: PathBuf) -> Self {
        Self {
            config,
            disable_marker_path,
        }
    }

    /// Idempotently materialize the ops channel without touching the opt-out
    /// marker.
    ///
    /// Reuses [`OpsLayout`] for the ops directory, pre-created component
    /// `.jsonl` files, the `instance.jsonl` snapshot, and logrotate policy.
    /// Deliberately does **not** clear the opt-out marker, so it is safe to run
    /// on every boot (`telemetry init`) without resurrecting a collection the
    /// user explicitly disabled.
    ///
    /// The snapshot is written only when `instance.jsonl` is empty or missing,
    /// so repeated calls (e.g. `telemetry enable` followed by systemd's
    /// `ExecStartPre=telemetry init`) do not produce duplicate lines.
    pub fn ensure_ops_channel(&self, linked: bool) -> Result<(), TelemetryError> {
        let ops = OpsLayout::new(&self.config);
        ops.create_ops_dir()?;
        ops.create_ops_jsonl_files()?;
        self.write_snapshot_if_empty(linked)?;
        ops.setup_logrotate()?;
        Ok(())
    }

    /// Enable collection: materialize the ops channel and clear the opt-out
    /// marker. Idempotent.
    ///
    /// Does not start the uploader loop; the caller pairs this with
    /// [`crate::telemetry::uploader::Uploader::ensure_running`].
    pub fn enable_collection(&self, linked: bool) -> Result<(), TelemetryError> {
        self.ensure_ops_channel(linked)?;
        if self.disable_marker_path.exists() {
            fs::remove_file(&self.disable_marker_path)?;
        }
        Ok(())
    }

    /// Append an instance snapshot for the current authorization state without
    /// touching the collection sentinel.
    ///
    /// Used by `telemetry link`: once the user authorizes named reporting, the
    /// now-permitted `instance_id` snapshot is recorded (when collection is
    /// already enabled) without implicitly enabling collection.
    pub fn append_instance_snapshot(&self, linked: bool) -> Result<(), TelemetryError> {
        let ops = OpsLayout::new(&self.config);
        ops.create_ops_dir()?;
        ops.create_ops_jsonl_files()?;
        self.write_snapshot(linked)?;
        Ok(())
    }

    /// Append one `instance.jsonl` line for the current `linked` state.
    ///
    /// Delegates the probe + serialization to [`crate::telemetry::instance`].
    /// Region and `instance_id` are not written here; the uploader injects them
    /// as common dimensions on every log line.
    fn write_snapshot(&self, linked: bool) -> Result<(), TelemetryError> {
        crate::telemetry::instance::write_instance_snapshot(&self.config, linked)?;
        Ok(())
    }

    /// Write a snapshot only when `instance.jsonl` is empty or missing.
    ///
    /// This makes [`ensure_ops_channel`] truly idempotent: the first call
    /// creates the snapshot, and subsequent calls (e.g. systemd's
    /// `ExecStartPre=telemetry init` triggered by `telemetry enable`) skip
    /// the write instead of appending a duplicate line. State-change writes
    /// (link / unlink) go through [`append_instance_snapshot`] which always
    /// appends.
    fn write_snapshot_if_empty(&self, linked: bool) -> Result<(), TelemetryError> {
        let path = self.config.ops_dir.join("instance.jsonl");
        let needs_write = match fs::read_to_string(&path) {
            Ok(content) => content.trim().is_empty(),
            Err(_) => true, // file missing → write
        };
        if needs_write {
            self.write_snapshot(linked)?;
        }
        Ok(())
    }

    /// Erase the persisted personal identity (`instance_id` / `uid`).
    ///
    /// Paired with `telemetry unlink`: withdrawing named-reporting consent must
    /// also remove the on-disk identifiers so the uploader stops attaching them
    /// (source minimization). Idempotent.
    pub fn forget_identity(&self) -> Result<(), TelemetryError> {
        crate::telemetry::instance::remove_identity_cache(&self.config)
    }

    /// Disable collection by writing the opt-out marker (mode 0644 so any
    /// component can `stat` it). Idempotent.
    ///
    /// Persistent user intent: the marker lives under `/etc/anolisa/` and is
    /// removed only by an explicit `enable` / `register`, so a reboot never
    /// resurrects collection. Does not delete already-buffered `.jsonl` data;
    /// a running uploader re-stats the marker each round and self-exits.
    pub fn disable_collection(&self) -> Result<(), TelemetryError> {
        if let Some(parent) = self.disable_marker_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.disable_marker_path, "")?;

        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.disable_marker_path, fs::Permissions::from_mode(0o644))?;
        }

        Ok(())
    }

    /// Whether collection is currently enabled (single uncached stat).
    ///
    /// Enabled by default; only an explicit opt-out marker disables it.
    pub fn is_enabled(&self) -> bool {
        !self.disable_marker_path.exists()
    }

    /// Path to the opt-out marker.
    pub fn disable_marker_path(&self) -> &Path {
        &self.disable_marker_path
    }
}

// ── Unit tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::test_config;
    use tempfile::TempDir;

    fn channel(dir: &TempDir) -> TelemetryChannel {
        TelemetryChannel::with_paths(test_config(dir), dir.path().join(".telemetry_disabled"))
    }

    #[test]
    fn test_enable_collection_creates_dir_files_and_clears_marker() {
        let dir = TempDir::new().unwrap();
        let ch = channel(&dir);
        // Start disabled to prove enable clears the opt-out marker.
        ch.disable_collection().unwrap();
        assert!(!ch.is_enabled());

        ch.enable_collection(false).unwrap();

        assert!(dir.path().join("ops").is_dir());
        let instance_jsonl = dir.path().join("ops/instance.jsonl");
        assert!(instance_jsonl.exists());
        // enable populates the instance snapshot, not just an empty file.
        let content = std::fs::read_to_string(&instance_jsonl).unwrap();
        assert!(!content.is_empty());
        // Unlinked: the personal instance_id must not be persisted.
        assert!(!content.contains("instance_id"));
        assert!(!content.contains("owner_account_id"));
        assert!(ch.is_enabled());
    }

    #[test]
    fn test_enable_collection_idempotent() {
        let dir = TempDir::new().unwrap();
        let ch = channel(&dir);
        ch.enable_collection(false).unwrap();
        ch.enable_collection(false).unwrap();
        assert!(ch.is_enabled());
    }

    #[test]
    fn test_enabled_by_default_without_marker() {
        let dir = TempDir::new().unwrap();
        let ch = channel(&dir);
        // No marker written → collection is on by default.
        assert!(ch.is_enabled());
    }

    #[test]
    fn test_ensure_ops_channel_preserves_disable_marker() {
        let dir = TempDir::new().unwrap();
        let ch = channel(&dir);
        ch.disable_collection().unwrap();
        assert!(!ch.is_enabled());

        // Boot-time self-heal must not resurrect a user's opt-out.
        ch.ensure_ops_channel(false).unwrap();
        assert!(!ch.is_enabled());
        assert!(dir.path().join("ops/instance.jsonl").exists());
    }

    #[test]
    fn test_ensure_ops_channel_does_not_duplicate_snapshot() {
        let dir = TempDir::new().unwrap();
        let ch = channel(&dir);

        // First call writes the initial snapshot.
        ch.ensure_ops_channel(false).unwrap();
        let path = dir.path().join("ops/instance.jsonl");
        let content = std::fs::read_to_string(&path).unwrap();
        let lines_after_first: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines_after_first.len(), 1);

        // Second call (e.g. systemd ExecStartPre=telemetry init) must not
        // append a duplicate line.
        ch.ensure_ops_channel(false).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let lines_after_second: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines_after_second.len(), 1);
    }

    #[test]
    fn test_append_instance_snapshot_records_identity_when_linked() {
        let dir = TempDir::new().unwrap();
        let ch = channel(&dir);
        ch.append_instance_snapshot(true).unwrap();

        // The snapshot itself does not duplicate instance_id / region (those
        // are uploader common dimensions). The identity cache is populated for
        // the uploader so it can inject instance_id / uid as common dimensions.
        let content = std::fs::read_to_string(dir.path().join("ops/instance.jsonl")).unwrap();
        assert!(content.contains("instance.source"));
        assert!(dir.path().join("identity.json").exists());
        // Linking records identity but must not write an opt-out marker.
        assert!(ch.is_enabled());
    }

    #[test]
    fn test_disable_collection_toggles_enabled() {
        let dir = TempDir::new().unwrap();
        let ch = channel(&dir);
        assert!(ch.is_enabled());

        ch.disable_collection().unwrap();
        assert!(!ch.is_enabled());

        // idempotent
        ch.disable_collection().unwrap();
        assert!(!ch.is_enabled());

        ch.enable_collection(false).unwrap();
        assert!(ch.is_enabled());
    }
}
