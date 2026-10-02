//! Container ID extraction from `/proc/{pid}/cgroup`.
//!
//! Standalone module (Footprint Ladder Level 3) because container detection
//! will expand to cover cgroup v2 unified hierarchy, runtime-specific
//! parsing, and optional container-name resolution via containerd API.
//! Keeping it separate from `ffi.rs` avoids bloating the FFI boundary file.
//!
//! Supports Docker (cgroup v1 & v2), containerd, and Kubernetes cgroup
//! layouts.  Returns `None` for non-container processes.

use lru::LruCache;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Maximum number of entries in the container-ID cache.
const CACHE_CAPACITY: usize = 256;

/// Time-to-live for a cache entry.
const CACHE_TTL: Duration = Duration::from_secs(60);

struct CacheEntry {
    container_id: Option<String>,
    inserted_at: Instant,
}

static CONTAINER_ID_CACHE: LazyLock<Mutex<LruCache<u32, CacheEntry>>> = LazyLock::new(|| {
    Mutex::new(LruCache::new(
        NonZeroUsize::new(CACHE_CAPACITY).expect("CACHE_CAPACITY > 0"),
    ))
});

/// Cached wrapper around [`extract_container_id`].
///
/// Returns the cached value if present and less than 60 seconds old.
/// On miss or expiry, calls `extract_container_id` and inserts the result.
/// Uses `lru::LruCache` for O(1) eviction, consistent with other agentsight
/// caches (HTTP aggregator, response map, id resolver).
pub fn extract_container_id_cached(pid: u32) -> Option<String> {
    let mut cache = match CONTAINER_ID_CACHE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };

    if let Some(entry) = cache.get(&pid) {
        if entry.inserted_at.elapsed() < CACHE_TTL {
            return entry.container_id.clone();
        }
    }

    let result = extract_container_id(pid);

    cache.put(
        pid,
        CacheEntry {
            container_id: result.clone(),
            inserted_at: Instant::now(),
        },
    );

    result
}

/// Read `/proc/{pid}/cgroup` and extract the container ID.
///
/// Returns `None` when the process is not running inside a container or
/// when the cgroup file cannot be read.
///
/// # Panic safety
///
/// This function and all callees are guaranteed no-panic: only infallible
/// string operations and `Option`-returning methods are used.  Safe to call
/// from FFI (`build_llm_data`).
pub fn extract_container_id(pid: u32) -> Option<String> {
    let path = crate::utils::procfs::proc_pid_entry(pid, "cgroup");
    match std::fs::read_to_string(&path) {
        Ok(content) => parse_container_id_from_cgroup(&content),
        Err(e) => {
            log::debug!("failed to read {path:?}: {e}");
            None
        }
    }
}

/// Pure function: extract a 64-char hex container ID from raw cgroup
/// file content.
///
/// Recognised layouts (checked in order):
///
/// 1. Docker cgroup v1 — `.../docker/<64hex>`
/// 2. Docker cgroup v2 — `docker-<64hex>.scope`
/// 3. Kubernetes       — `/kubepods/.../<64hex>`
/// 4. containerd       — last path segment is exactly 64 hex chars
/// 5. Kubernetes with the systemd cgroup driver —
///    `/kubepods.slice/.../<runtime>-<64hex>.scope` (cri-containerd-…, crio-…)
pub fn parse_container_id_from_cgroup(content: &str) -> Option<String> {
    for line in content.lines() {
        // The third colon-separated field is the cgroup path.
        let cgroup_path = match line.splitn(3, ':').nth(2) {
            Some(p) => p,
            None => continue,
        };

        if let Some(id) = try_extract_from_path(cgroup_path) {
            return Some(id);
        }
    }
    None
}

/// Try to extract a container ID from a single cgroup path string.
fn try_extract_from_path(path: &str) -> Option<String> {
    // 1. Docker cgroup v1: .../docker/<64hex>  (skip overlay2 layer paths)
    if let Some(pos) = path.find("/docker/") {
        let candidate = &path[pos + "/docker/".len()..];
        // split('/').next() always returns Some for non-empty input
        let candidate = candidate.split('/').next().unwrap_or("");
        if is_64_hex(candidate) {
            return Some(candidate.to_string());
        }
    }

    // 2. Docker cgroup v2: docker-<64hex>.scope
    for segment in path.rsplit('/') {
        if let Some(rest) = segment.strip_prefix("docker-") {
            if let Some(hex) = rest.strip_suffix(".scope") {
                if is_64_hex(hex) {
                    return Some(hex.to_string());
                }
            }
        }
    }

    // 3. Kubernetes: /kubepods/.../<64hex>
    if path.contains("/kubepods") {
        // rsplit('/').next() always returns Some (at least the full string)
        if let Some(segment) = path.rsplit('/').next() {
            if is_64_hex(segment) {
                return Some(segment.to_string());
            }
        }
    }

    // 4. containerd / generic: last path segment is exactly 64 hex chars.
    if let Some(segment) = path.rsplit('/').next() {
        if is_64_hex(segment) {
            return Some(segment.to_string());
        }
    }

    // 5. Kubernetes with the systemd cgroup driver: the container unit is
    //    `<runtime>-<64hex>.scope` under kubepods.slice (cri-containerd-…,
    //    crio-…; podman's libpod-… is covered too). Layouts 3/4 only match a
    //    bare <64hex> last segment (cgroupfs driver), so strip the ".scope"
    //    suffix and the runtime prefix (substring after the last dash).
    //    Gated on container markers so a host systemd unit never matches.
    if has_container_marker(path) {
        if let Some(segment) = path.rsplit('/').next() {
            let bare = segment.strip_suffix(".scope").unwrap_or(segment);
            if bare.len() > 64 {
                let candidate = bare.rsplit('-').next().unwrap_or("");
                if is_64_hex(candidate) {
                    return Some(candidate.to_string());
                }
            }
        }
    }

    None
}

/// Container runtime markers that gate the prefixed-scope rule (layout 5).
const CONTAINER_MARKERS: [&str; 7] = [
    "kubepods",
    "containerd",
    "crio",
    "docker",
    "libpod",
    "podman",
    "lxc",
];

fn has_container_marker(path: &str) -> bool {
    CONTAINER_MARKERS.iter().any(|marker| path.contains(marker))
}

/// Returns `true` when `s` is exactly 64 hex characters (case-insensitive).
fn is_64_hex(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

// ─── Storage persistence detection ───────────────────────────────────────────

/// Returns true when `path` resolves onto an overlay filesystem — a
/// container's writable layer, which is discarded when the container is
/// recreated (#2826).
///
/// Scope: this detects overlayfs only; other non-persistent mounts (e.g.
/// tmpfs-backed `emptyDir`) are deliberately out of scope — `emptyDir`
/// survives container restarts, which is the failure mode being flagged.
///
/// Reads `/proc/self/mounts`; returns false when the table is unreadable or
/// no mount matches (unknown is not overlay).
pub fn path_on_overlayfs(path: &Path) -> bool {
    // Resolve symlinks first: operators commonly symlink the data dir onto a
    // mounted volume, and matching the unresolved path would false-positive.
    // Fall back to the raw path when the directory does not exist yet.
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let Ok(mounts) = std::fs::read_to_string("/proc/self/mounts") else {
        return false;
    };
    path_on_overlayfs_in(&resolved, &mounts)
}

/// Warn when the data directory sits on non-persistent storage.
///
/// A container restart recreates the writable layer from the image, silently
/// wiping every database under the directory. Callers invoke this at startup
/// with the directory that will actually hold the databases; call sites are
/// once per process by construction (no dedup inside the function).
pub fn warn_if_data_dir_not_persistent(dir: &Path) {
    if path_on_overlayfs(dir) {
        log::warn!(
            "Data directory {} is on an overlay filesystem (container writable layer): \
             all captured data is discarded on every container restart. Mount a volume \
             (hostPath or PVC) at this path for persistence — see the deployment guide, \
             section \"Containers and sidecars\".",
            dir.display()
        );
    }
}

/// Evaluate a mount table (the text of `/proc/self/mounts`) for
/// [`path_on_overlayfs`]; kept separate so tests do not depend on the host.
fn path_on_overlayfs_in(path: &Path, mounts: &str) -> bool {
    let target = path.to_string_lossy();
    let mut best_len = 0usize;
    let mut overlay = false;
    for line in mounts.lines() {
        // Format: <src> <mountpoint> <fstype> <options> <dump> <pass>
        let mut fields = line.split_whitespace();
        let (Some(_src), Some(mount_point), Some(fstype)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        // The kernel octal-escapes spaces (and other specials) as \040.
        let mount_point = mount_point.replace("\\040", " ");
        let is_prefix = target == mount_point
            || target.starts_with(&format!("{mount_point}/"))
            || mount_point == "/";
        // Longest prefix wins; on ties the later entry wins, because stacked
        // mounts list the effective (most recently stacked) mount last.
        if is_prefix && mount_point.len() >= best_len {
            best_len = mount_point.len();
            overlay = fstype == "overlay";
        }
    }
    best_len > 0 && overlay
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── path_on_overlayfs ───────────────────────────────────────────────────

    #[test]
    fn overlayfs_detects_overlay_root() {
        let overlay_root = "overlay / overlay rw,relatime 0 0\nproc /proc proc rw 0 0\n";
        assert!(path_on_overlayfs_in(
            Path::new("/var/log/sysak/.agentsight"),
            overlay_root
        ));
    }

    #[test]
    fn overlayfs_hostpath_mount_wins_over_overlay_root() {
        // A hostPath/bind mount over the data dir makes it persistent even
        // when the root is overlay (longest-prefix match wins).
        let with_hostpath =
            "overlay / overlay rw 0 0\n/dev/sda1 /var/log/sysak/.agentsight ext4 rw 0 0\n";
        assert!(!path_on_overlayfs_in(
            Path::new("/var/log/sysak/.agentsight"),
            with_hostpath
        ));
    }

    #[test]
    fn overlayfs_plain_host_is_not_overlay() {
        let host = "/dev/sda1 / ext4 rw 0 0\nproc /proc proc rw 0 0\n";
        assert!(!path_on_overlayfs_in(
            Path::new("/var/log/sysak/.agentsight"),
            host
        ));
    }

    #[test]
    fn overlayfs_octal_escaped_space_mount_points() {
        // The kernel octal-escapes spaces in mount points (\040).
        let spaced =
            "overlay / overlay rw 0 0\n/dev/sda1 /var/log/sysak/with\\040space ext4 rw 0 0\n";
        assert!(
            !path_on_overlayfs_in(Path::new("/var/log/sysak/with space/db"), spaced),
            "ext4 bind mount must win over the overlay root"
        );
        assert!(path_on_overlayfs_in(Path::new("/other"), spaced));
    }

    #[test]
    fn overlayfs_stacked_same_mountpoint_last_wins() {
        // The same mount point mounted twice: the later (most recently
        // stacked) entry is the one in effect.
        let stacked = "overlay /data overlay rw 0 0\n/dev/sda1 /data ext4 rw 0 0\n";
        assert!(
            !path_on_overlayfs_in(Path::new("/data/db"), stacked),
            "the later stacked mount is the effective one"
        );
    }

    #[test]
    fn overlayfs_malformed_line_is_skipped() {
        // A line with fewer than three fields hits the continue branch.
        let mounts = "garbage\noverlay / overlay rw 0 0\n";
        assert!(path_on_overlayfs_in(Path::new("/x"), mounts));
    }

    #[test]
    fn overlayfs_empty_table_is_not_overlay() {
        assert!(!path_on_overlayfs_in(Path::new("/x"), ""));
    }

    #[test]
    fn overlayfs_wrapper_reads_proc_self_mounts() {
        // The wrapper must not panic regardless of the host topology; the
        // boolean outcome is host-dependent.
        let _ = path_on_overlayfs(Path::new("/"));
    }

    #[test]
    fn warn_helper_does_not_panic_on_any_path() {
        warn_if_data_dir_not_persistent(Path::new("/nonexistent-dir-for-test"));
        warn_if_data_dir_not_persistent(Path::new("/"));
    }

    #[test]
    fn docker_cgroup_v1() {
        let content =
            "12:devices:/docker/a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2\n";
        let id = parse_container_id_from_cgroup(content).unwrap();
        assert_eq!(
            id,
            "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
        );
    }

    #[test]
    fn docker_cgroup_v2() {
        let content = "0::/system.slice/docker-a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2.scope\n";
        let id = parse_container_id_from_cgroup(content).unwrap();
        assert_eq!(
            id,
            "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
        );
    }

    #[test]
    fn containerd() {
        let content =
            "0::/default/a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2\n";
        let id = parse_container_id_from_cgroup(content).unwrap();
        assert_eq!(
            id,
            "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
        );
    }

    #[test]
    fn kubernetes() {
        let content = "11:memory:/kubepods/burstable/pod1234/a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2\n";
        let id = parse_container_id_from_cgroup(content).unwrap();
        assert_eq!(
            id,
            "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
        );
    }

    #[test]
    fn kubernetes_containerd_systemd_cgroup_v2() {
        let content = "0::/kubepods.slice/kubepods-burstable.slice/kubepods-burstable-pod27e3a4f0_0000.slice/cri-containerd-a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2.scope\n";
        let id = parse_container_id_from_cgroup(content).unwrap();
        assert_eq!(
            id,
            "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
        );
    }

    #[test]
    fn kubernetes_containerd_systemd_cgroup_v1() {
        let content = "11:memory:/kubepods.slice/kubepods-burstable.slice/kubepods-burstable-pod27e3a4f0_0000.slice/cri-containerd-a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2.scope\n";
        let id = parse_container_id_from_cgroup(content).unwrap();
        assert_eq!(
            id,
            "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
        );
    }

    #[test]
    fn kubernetes_crio_systemd() {
        let content = "0::/kubepods.slice/kubepods-besteffort.slice/kubepods-besteffort-pod0000.slice/crio-a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2.scope\n";
        let id = parse_container_id_from_cgroup(content).unwrap();
        assert_eq!(
            id,
            "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
        );
    }

    #[test]
    fn podman_libpod_scope() {
        let content = "0::/user.slice/user-1000.slice/user@1000.service/user.slice/libpod-a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2.scope\n";
        let id = parse_container_id_from_cgroup(content).unwrap();
        assert_eq!(
            id,
            "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
        );
    }

    #[test]
    fn host_systemd_unit_with_hex_tail_is_not_container() {
        // No container marker in the path: a host systemd unit whose tail
        // happens to be 64 hex chars must not be mistaken for a container.
        let content = "0::/system.slice/weird-a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2.scope\n";
        assert!(parse_container_id_from_cgroup(content).is_none());
    }

    #[test]
    fn non_container_host_process() {
        let content = "12:devices:/user.slice/user-1000.slice/session-1.scope\n\
                        11:memory:/user.slice\n\
                        0::/init.scope\n";
        assert!(parse_container_id_from_cgroup(content).is_none());
    }

    #[test]
    fn empty_content() {
        assert!(parse_container_id_from_cgroup("").is_none());
    }

    #[test]
    fn multiline_picks_first_match() {
        let content = "12:devices:/\n\
                        11:memory:/docker/a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2\n\
                        0::/system.slice\n";
        let id = parse_container_id_from_cgroup(content).unwrap();
        assert_eq!(
            id,
            "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
        );
    }

    #[test]
    fn short_hex_is_not_container_id() {
        // 32 chars — too short for a container ID
        let content = "0::/docker/a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4\n";
        assert!(parse_container_id_from_cgroup(content).is_none());
    }

    #[test]
    fn overlay2_is_not_container_id() {
        // overlay2 layer IDs are long hex but sit under /docker/overlay2/, not /docker/<id>
        let content = "0::/system.slice/docker-abcdef.scope/docker/overlay2/a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2/merged\n";
        // Should match the docker-abcdef.scope pattern (if 64 hex), NOT the overlay2 layer
        // In this case docker-abcdef.scope only has 6 hex chars, so no match at all
        assert!(parse_container_id_from_cgroup(content).is_none());
    }

    #[test]
    fn uppercase_hex_accepted() {
        let content =
            "0::/docker/A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2\n";
        let id = parse_container_id_from_cgroup(content).unwrap();
        assert_eq!(
            id,
            "A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2"
        );
    }

    #[test]
    fn no_trailing_newline() {
        let content = "0::/docker/a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
        let id = parse_container_id_from_cgroup(content).unwrap();
        assert_eq!(
            id,
            "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
        );
    }

    #[test]
    fn test_cache_returns_same_value_on_second_call() {
        // Call twice with the same pid — both should return the same value
        // and neither should panic.
        let pid = std::process::id();
        let first = extract_container_id_cached(pid);
        let second = extract_container_id_cached(pid);
        assert_eq!(first, second);
    }

    #[test]
    fn test_cache_none_for_nonexistent_pid() {
        // pid 999999 almost certainly does not exist; should return None
        // without panicking.
        let result = extract_container_id_cached(999_999);
        assert!(result.is_none());
    }
}
