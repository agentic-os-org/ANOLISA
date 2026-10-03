//! Backend-agnostic helpers: LC-locked command execution, mount probing, symlink recovery.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tracing::{debug, info, warn};

use crate::state::DaemonState;

/// Run a command and return stdout; non-zero exit is a hard failure.
///
/// Forces `LC_ALL=C LANG=C` so parsers (df, losetup -j, ...) see canonical output.
pub async fn run_command(cmd: &str, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new(cmd)
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .args(args)
        .output()
        .await
        .with_context(|| format!("Failed to execute: {} {:?}", cmd, args))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Command `{} {:?}` failed with status {}: {}",
            cmd,
            args,
            output.status,
            stderr.trim()
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Same as `run_command` but discards stdout.
pub async fn run_command_checked(cmd: &str, args: &[&str]) -> anyhow::Result<()> {
    run_command(cmd, args).await?;
    Ok(())
}

/// Decode `\NNN` octal escapes used by /proc/mounts for whitespace and
/// backslashes in mount-point paths (e.g. space → \040, tab → \011).
/// Unrecognised sequences are left literal so a malformed line never panics.
pub fn unescape_proc_mount(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let d0 = bytes[i + 1];
            let d1 = bytes[i + 2];
            let d2 = bytes[i + 3];
            if (b'0'..=b'7').contains(&d0)
                && (b'0'..=b'7').contains(&d1)
                && (b'0'..=b'7').contains(&d2)
            {
                let v = ((d0 - b'0') << 6) | ((d1 - b'0') << 3) | (d2 - b'0');
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Return true if `mount_path` appears in `/proc/mounts`.
pub async fn is_mounted(mount_path: &str) -> anyhow::Result<bool> {
    let target = Path::new(mount_path);
    let target_norm = target.components().collect::<PathBuf>();

    let file = File::open("/proc/mounts")
        .await
        .context("Failed to open /proc/mounts")?;
    let mut reader = BufReader::new(file).lines();

    while let Some(line) = reader.next_line().await? {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if let Some(mp) = parts.get(1) {
            let decoded = unescape_proc_mount(mp);
            let mp_path = Path::new(&decoded);
            if mp_path == target || mp_path.components().collect::<PathBuf>() == target_norm {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

/// Ensure every registered workspace's user-facing path is a symlink pointing at
/// `data_root/<ws_id>`; rebuild if missing or wrong target.
pub async fn ensure_symlinks(state: &DaemonState) {
    let all_ws = state.all_workspaces();
    for arc in all_ws {
        let ws = arc.read().await;
        let expected_subvol_path = state.backend.data_root().join(&ws.ws_id);
        let ws_path = ws.path.to_string_lossy().to_string();

        // Guard against dangling symlinks when the subvolume is missing.
        if !expected_subvol_path.exists() {
            warn!(
                "subvolume {:?} missing for workspace {}; skipping symlink recovery",
                expected_subvol_path, ws.ws_id
            );
            continue;
        }

        match tokio::fs::read_link(&ws_path).await {
            Ok(target) if target == expected_subvol_path => {
                debug!("symlink OK for {}: -> {:?}", ws_path, target);
            }
            Ok(target) => {
                warn!(
                    "symlink {} points to {:?}, expected {:?}; rebuilding",
                    ws_path, target, expected_subvol_path
                );
                rebuild_symlink(&ws_path, &expected_subvol_path).await;
            }
            Err(_) => {
                warn!("symlink missing or invalid for {}; rebuilding", ws_path);
                rebuild_symlink(&ws_path, &expected_subvol_path).await;
            }
        }
    }
}

/// Atomically replace the symlink via temp-file + rename.
async fn rebuild_symlink(ws_path: &str, expected_subvol_path: &Path) {
    let tmp_path = format!("{}.tmp", ws_path);
    // Best-effort cleanup of leftover residue from a prior daemon crash between
    // symlink() and rename(); without this, symlink() returns EEXIST and
    // recovery wedges permanently for this workspace.
    let _ = tokio::fs::remove_file(&tmp_path).await;
    if let Err(e) = tokio::fs::symlink(expected_subvol_path, &tmp_path).await {
        warn!("failed to create temp symlink for {}: {}", ws_path, e);
        return;
    }
    if let Err(e) = tokio::fs::rename(&tmp_path, ws_path).await {
        warn!(
            "failed to atomically replace symlink for {}: {}",
            ws_path, e
        );
        let _ = tokio::fs::remove_file(&tmp_path).await;
    } else {
        info!("rebuilt symlink for {}", ws_path);
    }
}

// cwd occupant guard.
//
// Scans /proc for processes whose cwd resolves into the workspace (including
// bind-mount aliases derived from /proc/self/mountinfo). Cannot detect
// cross-namespace occupants: /proc/<pid>/cwd reports paths in the target's
// own mount namespace, which are not comparable to the daemon's view.

#[derive(Debug)]
pub(crate) enum CwdScanError {
    Canonicalize(std::io::Error),
    ProcRead(std::io::Error),
    MountinfoRead(std::io::Error),
}

impl std::fmt::Display for CwdScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CwdScanError::Canonicalize(e) => {
                write!(f, "canonicalize workspace failed: {}", e)
            }
            CwdScanError::ProcRead(e) => write!(f, "read /proc failed: {}", e),
            CwdScanError::MountinfoRead(e) => {
                write!(f, "read /proc/self/mountinfo failed: {}", e)
            }
        }
    }
}

#[derive(Debug, Clone)]
struct MountEntry {
    dev: String,
    fs_root: String,
    mountpoint: String,
}

/// Parse /proc/self/mountinfo: `mount_id parent_id major:minor root mountpoint ...`
fn parse_mountinfo(content: &str) -> Vec<MountEntry> {
    let mut out = Vec::new();
    for line in content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 5 {
            continue;
        }
        let dev = parts[2].to_string();
        let fs_root = unescape_proc_mount(parts[3]);
        let mountpoint = unescape_proc_mount(parts[4]);
        if !dev.contains(':') || !fs_root.starts_with('/') || !mountpoint.starts_with('/') {
            continue;
        }
        out.push(MountEntry {
            dev,
            fs_root,
            mountpoint,
        });
    }
    out
}

/// All absolute paths through which `ws_abs` is reachable in the current
/// mount namespace (bind-mount aliases included). Always contains `ws_abs`.
fn derive_workspace_aliases(ws_abs: &str, mounts: &[MountEntry]) -> Vec<String> {
    let mut aliases = vec![ws_abs.to_string()];

    let host = mounts
        .iter()
        .filter(|m| {
            m.mountpoint == ws_abs
                || ws_abs.starts_with(&format!("{}/", m.mountpoint.trim_end_matches('/')))
        })
        .max_by_key(|m| m.mountpoint.len());
    let Some(host) = host else { return aliases };

    let suffix = if ws_abs == host.mountpoint {
        String::new()
    } else {
        ws_abs[host.mountpoint.len()..]
            .trim_start_matches('/')
            .to_string()
    };
    let ws_inside_fs = if host.fs_root == "/" {
        format!("/{}", suffix)
    } else if suffix.is_empty() {
        host.fs_root.clone()
    } else {
        format!("{}/{}", host.fs_root.trim_end_matches('/'), suffix)
    };
    let ws_inside_fs = ws_inside_fs.trim_end_matches('/').to_string();
    let ws_inside_fs = if ws_inside_fs.is_empty() {
        "/".to_string()
    } else {
        ws_inside_fs
    };

    for m in mounts {
        if m.dev != host.dev {
            continue;
        }
        let covers = m.fs_root == ws_inside_fs
            || ws_inside_fs.starts_with(&format!("{}/", m.fs_root.trim_end_matches('/')));
        if !covers {
            continue;
        }
        let inner = if m.fs_root == ws_inside_fs {
            String::new()
        } else if m.fs_root == "/" {
            ws_inside_fs.trim_start_matches('/').to_string()
        } else {
            ws_inside_fs[m.fs_root.trim_end_matches('/').len()..]
                .trim_start_matches('/')
                .to_string()
        };
        let alias = if inner.is_empty() {
            m.mountpoint.clone()
        } else if m.mountpoint == "/" {
            format!("/{}", inner)
        } else {
            format!("{}/{}", m.mountpoint.trim_end_matches('/'), inner)
        };
        if !aliases.contains(&alias) {
            aliases.push(alias);
        }
    }

    aliases
}

async fn read_mountinfo() -> Result<Vec<MountEntry>, CwdScanError> {
    let content = tokio::fs::read_to_string("/proc/self/mountinfo")
        .await
        .map_err(CwdScanError::MountinfoRead)?;
    Ok(parse_mountinfo(&content))
}

fn cwd_matches_any_alias(cwd: &str, aliases: &[String]) -> bool {
    for alias in aliases {
        if cwd == alias {
            return true;
        }
        let prefix = if alias.ends_with('/') {
            alias.clone()
        } else {
            format!("{}/", alias)
        };
        if cwd.starts_with(&prefix) {
            return true;
        }
    }
    false
}

/// Per-entry errors are skipped (PID exit / EPERM are normal on /proc churn);
/// scan-level failures return `CwdScanError` and the caller must treat the
/// scan as inconclusive.
pub(crate) async fn find_cwd_occupants(
    workspace: &str,
) -> Result<Vec<(u32, String)>, CwdScanError> {
    let ws = tokio::fs::canonicalize(workspace)
        .await
        .map_err(CwdScanError::Canonicalize)?;
    let ws_str = ws.to_string_lossy().to_string();

    let mounts = read_mountinfo().await?;
    let aliases = derive_workspace_aliases(&ws_str, &mounts);

    let mut occupants = Vec::new();
    let mut entries = tokio::fs::read_dir("/proc")
        .await
        .map_err(CwdScanError::ProcRead)?;

    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(e)) => e,
            Ok(None) => break,
            Err(_) => continue,
        };

        let name = entry.file_name();
        let Ok(pid) = name.to_string_lossy().parse::<u32>() else {
            continue;
        };

        let Ok(target) = tokio::fs::read_link(format!("/proc/{}/cwd", pid)).await else {
            continue;
        };
        let target_str = target.to_string_lossy().to_string();
        // Orphan cwd from a previously crashed swap: kernel marks the dentry
        // " (deleted)". The process is already broken; a new swap can't make
        // it worse, so guard stays purely preventive and skips it. Must skip
        // before the matcher — "/ws/src (deleted)" would otherwise prefix-
        // match alias "/ws".
        if target_str.ends_with(" (deleted)") {
            continue;
        }
        if cwd_matches_any_alias(&target_str, &aliases) {
            occupants.push((pid, target_str));
        }
    }

    Ok(occupants)
}

/// Returns None (proceed) or an error response.
/// Confirmed occupants → `CwdOccupied`; scan failures → `CwdScanFailed`.
pub(crate) async fn guard_cwd_occupants(workspace: &str) -> Option<ws_ckpt_common::Response> {
    let occupants = match find_cwd_occupants(workspace).await {
        Ok(list) => list,
        Err(err) => {
            warn!(
                "cwd scan failed for {}: {} — refusing (fail-closed)",
                workspace, err
            );
            return Some(ws_ckpt_common::Response::Error {
                code: ws_ckpt_common::ErrorCode::CwdScanFailed,
                message: format!(
                    "Refused: cwd scan failed ({}). \
                     The /proc scan could not complete, so occupant status is unknown; \
                     this is typically transient and the operation may succeed on retry.",
                    err
                ),
            });
        }
    };

    if occupants.is_empty() {
        return None;
    }

    let detail: Vec<String> = occupants
        .iter()
        .map(|(pid, cwd)| format!("PID {} (cwd={})", pid, cwd))
        .collect();

    warn!(
        "cwd occupant guard: {} process(es) with cwd inside {}: [{}]",
        occupants.len(),
        workspace,
        detail.join(", ")
    );

    Some(ws_ckpt_common::Response::Error {
        code: ws_ckpt_common::ErrorCode::CwdOccupied,
        message: format!(
            "Refused: {} process(es) have cwd inside workspace: {}. \
             Symlink swap would invalidate their cwd. \
             Move affected processes out of the workspace before retrying.",
            occupants.len(),
            detail.join("; "),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn is_mounted_detects_real_mount_point() {
        // `/proc` is a mount point on every supported (Linux) host, and a fresh
        // temp dir never is. Guards the workspace-root check in `init`.
        assert!(is_mounted("/proc").await.unwrap());
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_mounted(dir.path().to_str().unwrap()).await.unwrap());
    }

    #[tokio::test]
    async fn is_mounted_ignores_path_below_mount_point() {
        // Only the mount point itself blocks rename(2); descendants are fine.
        assert!(!is_mounted("/proc/self").await.unwrap());
    }

    #[test]
    fn unescape_space_and_tab() {
        assert_eq!(unescape_proc_mount("/mnt/my\\040dir"), "/mnt/my dir");
        assert_eq!(unescape_proc_mount("/a\\011b"), "/a\tb");
    }

    #[test]
    fn unescape_backslash() {
        assert_eq!(unescape_proc_mount("/path\\134name"), "/path\\name");
    }

    #[test]
    fn unescape_passthrough_plain_ascii() {
        assert_eq!(unescape_proc_mount("/var/lib/ws-ckpt"), "/var/lib/ws-ckpt");
    }

    #[test]
    fn unescape_incomplete_sequence_left_literal() {
        // Trailing `\04` has only 2 digits — not a valid octal triple.
        assert_eq!(unescape_proc_mount("/end\\04"), "/end\\04");
    }

    #[test]
    fn unescape_non_octal_digit_left_literal() {
        // `\089` — '8' is not octal, sequence left untouched.
        assert_eq!(unescape_proc_mount("/x\\089"), "/x\\089");
    }

    #[test]
    fn parse_mountinfo_basic_line() {
        let line = "36 35 8:1 /home/admin /elsewhere rw,relatime shared:5 - ext4 /dev/sda1 rw";
        let entries = parse_mountinfo(line);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].dev, "8:1");
        assert_eq!(entries[0].fs_root, "/home/admin");
        assert_eq!(entries[0].mountpoint, "/elsewhere");
    }

    #[test]
    fn parse_mountinfo_skips_malformed_lines() {
        let input = "\
36 35 8:1 / /mnt rw - ext4 /dev/sda1 rw
not enough fields
123 broken dev field /a /b
37 35 8:1 / /other rw - ext4 /dev/sda1 rw
";
        let entries = parse_mountinfo(input);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].mountpoint, "/mnt");
        assert_eq!(entries[1].mountpoint, "/other");
    }

    #[test]
    fn parse_mountinfo_decodes_octal_escapes() {
        let line = "36 35 8:1 /home/with\\040space /mnt/dir\\040name rw - ext4 /dev/sda1 rw";
        let entries = parse_mountinfo(line);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].fs_root, "/home/with space");
        assert_eq!(entries[0].mountpoint, "/mnt/dir name");
    }

    fn mount(dev: &str, fs_root: &str, mountpoint: &str) -> MountEntry {
        MountEntry {
            dev: dev.to_string(),
            fs_root: fs_root.to_string(),
            mountpoint: mountpoint.to_string(),
        }
    }

    #[test]
    fn aliases_canonical_path_when_no_binds() {
        let mounts = vec![mount("8:1", "/", "/")];
        let aliases = derive_workspace_aliases("/home/admin", &mounts);
        assert_eq!(aliases, vec!["/home/admin".to_string()]);
    }

    #[test]
    fn aliases_detects_simple_bind_mount() {
        let mounts = vec![
            mount("8:1", "/", "/"),
            mount("8:1", "/home/admin", "/elsewhere"),
        ];
        let aliases = derive_workspace_aliases("/home/admin", &mounts);
        assert!(aliases.contains(&"/home/admin".to_string()));
        assert!(aliases.contains(&"/elsewhere".to_string()));
    }

    #[test]
    fn aliases_detects_ancestor_bind_mount() {
        // mount --bind /home /mnt/home → workspace /home/admin becomes /mnt/home/admin
        let mounts = vec![mount("8:1", "/", "/"), mount("8:1", "/home", "/mnt/home")];
        let aliases = derive_workspace_aliases("/home/admin", &mounts);
        assert!(aliases.contains(&"/mnt/home/admin".to_string()));
    }

    #[test]
    fn aliases_separate_filesystem_mounted_workspace() {
        // Workspace on separate fs; bind-exposed at /backup.
        let mounts = vec![
            mount("8:1", "/", "/"),
            mount("8:32", "/", "/data"),
            mount("8:32", "/", "/backup"),
        ];
        let aliases = derive_workspace_aliases("/data/ws", &mounts);
        assert!(aliases.contains(&"/backup/ws".to_string()));
    }

    #[test]
    fn aliases_ignores_different_device() {
        let mounts = vec![
            mount("8:1", "/", "/"),
            mount("8:99", "/home/admin", "/decoy"),
        ];
        let aliases = derive_workspace_aliases("/home/admin", &mounts);
        assert!(!aliases.contains(&"/decoy".to_string()));
    }

    #[test]
    fn cwd_matches_alias_exact_and_subdir() {
        let aliases = vec!["/home/admin".to_string(), "/elsewhere".to_string()];
        assert!(cwd_matches_any_alias("/home/admin", &aliases));
        assert!(cwd_matches_any_alias("/home/admin/src", &aliases));
        assert!(cwd_matches_any_alias("/elsewhere", &aliases));
        assert!(cwd_matches_any_alias("/elsewhere/deep/path", &aliases));
    }

    #[test]
    fn cwd_does_not_match_sibling_prefix() {
        // Regression: /home/administrator must not match alias /home/admin.
        let aliases = vec!["/home/admin".to_string()];
        assert!(!cwd_matches_any_alias("/home/administrator", &aliases));
        assert!(!cwd_matches_any_alias("/home/administrator/foo", &aliases));
    }

    #[test]
    fn cwd_does_not_match_unrelated_path() {
        let aliases = vec!["/home/admin".to_string()];
        assert!(!cwd_matches_any_alias("/tmp", &aliases));
        assert!(!cwd_matches_any_alias("/", &aliases));
    }

    #[test]
    fn orphan_cwd_requires_external_skip() {
        // Bare matcher would match subdirectory orphans — caller must skip
        // " (deleted)" before invoking the matcher.
        let aliases = vec!["/home/admin".to_string()];
        assert!(cwd_matches_any_alias("/home/admin/src (deleted)", &aliases));
        assert!("/home/admin/src (deleted)".ends_with(" (deleted)"));
    }
}
