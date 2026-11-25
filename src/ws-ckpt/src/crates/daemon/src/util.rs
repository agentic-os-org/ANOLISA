//! Backend-agnostic helpers: LC-locked command execution, mount probing, symlink recovery.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
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

/// A decoded mount-table entry shared by mount detection and validation.
#[derive(Debug)]
pub(crate) struct ProcMount {
    pub(crate) device: String,
    pub(crate) mount_point: PathBuf,
    pub(crate) fs_type: String,
    pub(crate) read_write: bool,
}

impl ProcMount {
    /// Whether this entry is eligible for automatic BtrfsBase selection.
    pub(crate) fn is_writable_physical_btrfs(&self) -> bool {
        self.fs_type == "btrfs"
            && self.read_write
            && self.device.starts_with("/dev/")
            && !self.device.starts_with("/dev/loop")
    }
}

/// Decode effective mounts in table order, keeping the last exact stacked entry.
pub(crate) fn proc_mounts_in(content: &str) -> Vec<ProcMount> {
    let mut seen = std::collections::HashSet::new();
    let mut mounts: Vec<_> = content
        .lines()
        .rev()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let device = fields.next()?;
            let mount_point = fields.next()?;
            let fs_type = fields.next()?;
            let options = fields.next()?;
            let mount_point = Path::new(&unescape_proc_mount(mount_point))
                .components()
                .collect::<PathBuf>();
            if !seen.insert(mount_point.clone()) {
                return None;
            }
            Some(ProcMount {
                device: unescape_proc_mount(device),
                mount_point,
                fs_type: fs_type.to_string(),
                read_write: options.split(',').any(|option| option == "rw")
                    && !options.split(',').any(|option| option == "ro"),
            })
        })
        .collect();
    mounts.reverse();
    mounts
}

/// Return true if `mount_path` appears as an exact effective `/proc/mounts` entry.
pub async fn is_mounted(mount_path: &str) -> anyhow::Result<bool> {
    let content = tokio::fs::read_to_string("/proc/mounts")
        .await
        .context("Failed to read /proc/mounts")?;
    let target = Path::new(mount_path).components().collect::<PathBuf>();
    Ok(proc_mounts_in(&content)
        .iter()
        .any(|entry| entry.mount_point == target))
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
    mount_id: u64,
    parent_id: u64,
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
        let Ok(mount_id) = parts[0].parse::<u64>() else {
            continue;
        };
        let Ok(parent_id) = parts[1].parse::<u64>() else {
            continue;
        };
        let dev = parts[2].to_string();
        let fs_root = unescape_proc_mount(parts[3]);
        let mountpoint = unescape_proc_mount(parts[4]);
        if !dev.contains(':') || !fs_root.starts_with('/') || !mountpoint.starts_with('/') {
            continue;
        }
        out.push(MountEntry {
            mount_id,
            parent_id,
            dev,
            fs_root,
            mountpoint,
        });
    }
    out
}

fn normalize_mount_path(path: &str) -> &str {
    if path.is_empty() {
        return "";
    }
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/"
    } else {
        trimmed
    }
}

fn path_is_same_or_descendant(path: &str, ancestor: &str) -> bool {
    let path = normalize_mount_path(path);
    let ancestor = normalize_mount_path(ancestor);
    if ancestor == "/" {
        return path.starts_with('/');
    }
    if path == ancestor {
        return true;
    }
    path.len() > ancestor.len()
        && path.starts_with(ancestor)
        && path.as_bytes()[ancestor.len()] == b'/'
}

/// Return only mounts currently visible in the mount namespace, filtering out
/// mounts shadowed by stacked overmounts or overmounts on an ancestor path.
fn visible_mounts(mounts: &[MountEntry]) -> Vec<&MountEntry> {
    let mount_ids: std::collections::HashSet<_> = mounts.iter().map(|m| m.mount_id).collect();

    let mut children: std::collections::HashMap<u64, Vec<usize>> = std::collections::HashMap::new();
    let mut roots = Vec::new();
    for (idx, mount) in mounts.iter().enumerate() {
        if mount_ids.contains(&mount.parent_id) && mount.parent_id != mount.mount_id {
            children.entry(mount.parent_id).or_default().push(idx);
        } else {
            roots.push(idx);
        }
    }

    let active_roots: Vec<usize> = roots
        .iter()
        .enumerate()
        .filter(|(pos, &idx)| {
            !roots[pos + 1..].iter().any(|&later_idx| {
                path_is_same_or_descendant(&mounts[idx].mountpoint, &mounts[later_idx].mountpoint)
            })
        })
        .map(|(_, &idx)| idx)
        .collect();

    let mut visible_indices = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut stack = active_roots;

    while let Some(mut cur) = stack.pop() {
        if !visited.insert(cur) {
            continue;
        }
        // Follow any stacked overmounts attached directly at `cur.mountpoint`.
        loop {
            let cur_id = mounts[cur].mount_id;
            let cur_mp = normalize_mount_path(&mounts[cur].mountpoint);
            let stacked = children.get(&cur_id).and_then(|kids| {
                kids.iter()
                    .copied()
                    .rfind(|&k| normalize_mount_path(&mounts[k].mountpoint) == cur_mp)
            });
            match stacked {
                Some(next) if visited.insert(next) => cur = next,
                _ => break,
            }
        }

        visible_indices.push(cur);

        let cur_id = mounts[cur].mount_id;
        let cur_mp = normalize_mount_path(&mounts[cur].mountpoint);
        if let Some(kids) = children.get(&cur_id) {
            let non_stacked: Vec<usize> = kids
                .iter()
                .copied()
                .filter(|&k| normalize_mount_path(&mounts[k].mountpoint) != cur_mp)
                .collect();
            for (pos, &k) in non_stacked.iter().enumerate() {
                let k_mp = normalize_mount_path(&mounts[k].mountpoint);
                let hidden = non_stacked[pos + 1..].iter().any(|&later| {
                    let later_mp = normalize_mount_path(&mounts[later].mountpoint);
                    path_is_same_or_descendant(k_mp, later_mp)
                });
                if !hidden {
                    stack.push(k);
                }
            }
        }
    }

    visible_indices.sort_unstable();
    visible_indices.into_iter().map(|i| &mounts[i]).collect()
}

fn resolve_fs_path(path: &str, mount: &MountEntry) -> String {
    let norm_path = normalize_mount_path(path);
    let norm_mp = normalize_mount_path(&mount.mountpoint);
    let norm_root = normalize_mount_path(&mount.fs_root);

    let suffix = if norm_path == norm_mp {
        ""
    } else if norm_mp == "/" {
        norm_path.trim_start_matches('/')
    } else {
        norm_path[norm_mp.len()..].trim_start_matches('/')
    };

    if suffix.is_empty() {
        norm_root.to_string()
    } else if norm_root == "/" {
        format!("/{}", suffix)
    } else {
        format!("{}/{}", norm_root, suffix)
    }
}

fn effective_mount<'a>(path: &str, visible: &[&'a MountEntry]) -> Option<&'a MountEntry> {
    visible
        .iter()
        .copied()
        .filter(|m| path_is_same_or_descendant(path, &m.mountpoint))
        .max_by_key(|m| normalize_mount_path(&m.mountpoint).len())
}

fn resolve_path_identity<'a>(path: &str, visible: &[&'a MountEntry]) -> Option<(&'a str, String)> {
    let mount = effective_mount(path, visible)?;
    Some((mount.dev.as_str(), resolve_fs_path(path, mount)))
}

fn derive_workspace_aliases_from_visible(
    ws_abs: &str,
    visible: &[&MountEntry],
    ws_identity: Option<&(&str, String)>,
) -> Vec<String> {
    let mut aliases = vec![ws_abs.to_string()];

    let Some(&(ws_dev, ref ws_inside_fs)) = ws_identity else {
        return aliases;
    };

    for m in visible {
        if m.dev != ws_dev {
            continue;
        }
        let m_fs_root = normalize_mount_path(&m.fs_root);
        let m_mp = normalize_mount_path(&m.mountpoint);
        let covers = path_is_same_or_descendant(ws_inside_fs, m_fs_root);
        // A visible bind mount whose fs_root sits strictly inside ws_inside_fs
        // exposes a workspace subdirectory directly at m.mountpoint.
        let is_subpath = !covers && path_is_same_or_descendant(m_fs_root, ws_inside_fs);
        let alias = if is_subpath {
            m_mp.to_string()
        } else if covers {
            let inner = if m_fs_root == ws_inside_fs {
                ""
            } else if m_fs_root == "/" {
                ws_inside_fs.trim_start_matches('/')
            } else {
                ws_inside_fs[m_fs_root.len()..].trim_start_matches('/')
            };
            if inner.is_empty() {
                m_mp.to_string()
            } else if m_mp == "/" {
                format!("/{}", inner)
            } else {
                format!("{}/{}", m_mp, inner)
            }
        } else {
            continue;
        };
        // Drop candidate aliases whose path is currently covered by an
        // unrelated mount (for example, an ancestor bind mount whose workspace
        // subpath is overmounted).
        let Some((alias_dev, alias_fs)) = resolve_path_identity(&alias, visible) else {
            continue;
        };
        if alias_dev != ws_dev || !path_is_same_or_descendant(&alias_fs, ws_inside_fs) {
            continue;
        }
        if !aliases.contains(&alias) {
            aliases.push(alias);
        }
    }

    aliases
}

struct WorkspaceCwdMatcher<'a> {
    aliases: Vec<String>,
    visible: Vec<&'a MountEntry>,
    ws_identity: Option<(&'a str, String)>,
}

impl<'a> WorkspaceCwdMatcher<'a> {
    fn new(ws_abs: &str, mounts: &'a [MountEntry]) -> Self {
        let visible = visible_mounts(mounts);
        let ws_identity = resolve_path_identity(ws_abs, &visible);
        let aliases = derive_workspace_aliases_from_visible(ws_abs, &visible, ws_identity.as_ref());
        Self {
            aliases,
            visible,
            ws_identity,
        }
    }

    fn matches(&self, cwd: &str) -> bool {
        if !cwd_matches_any_alias(cwd, &self.aliases) {
            return false;
        }
        let Some((ws_dev, ws_inside_fs)) = &self.ws_identity else {
            return true;
        };
        let Some((cwd_dev, cwd_inside_fs)) = resolve_path_identity(cwd, &self.visible) else {
            return false;
        };
        cwd_dev == *ws_dev && path_is_same_or_descendant(&cwd_inside_fs, ws_inside_fs)
    }
}

/// All visible absolute paths through which `ws_abs` is reachable in the
/// current mount namespace (bind-mount aliases included). Always contains
/// `ws_abs`.
#[cfg(test)]
fn derive_workspace_aliases(ws_abs: &str, mounts: &[MountEntry]) -> Vec<String> {
    WorkspaceCwdMatcher::new(ws_abs, mounts).aliases
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
    let matcher = WorkspaceCwdMatcher::new(&ws_str, &mounts);

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
        if matcher.matches(&target_str) {
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

    #[test]
    fn proc_mount_parser_decodes_paths_options_and_visible_stacked_entries() {
        let content = "malformed\n/dev/old /with\\040space btrfs rw 0 0\n/dev/mapper/new\\134name /with\\040space/./ btrfs ro,rw 0 0\n/dev/sdb /tab\\011path btrfs rw,relatime 0 0\n/dev/loop0 /loop btrfs rw 0 0\nnone /nondevice btrfs rw 0 0\n/dev/sdc /prefix btrfs rwish 0 0\n";
        let entries = proc_mounts_in(content);
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0].device, "/dev/mapper/new\\name");
        assert_eq!(entries[0].mount_point, Path::new("/with space"));
        assert!(!entries[0].read_write, "ro must win over rw");
        assert!(!entries[0].is_writable_physical_btrfs());
        assert_eq!(entries[1].mount_point, Path::new("/tab\tpath"));
        assert!(entries[1].is_writable_physical_btrfs());
        assert!(!entries[2].is_writable_physical_btrfs());
        assert!(!entries[3].is_writable_physical_btrfs());
        assert!(!entries[4].read_write);
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
        assert_eq!(entries[0].mount_id, 36);
        assert_eq!(entries[0].parent_id, 35);
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

    #[test]
    fn aliases_canonical_path_when_no_binds() {
        let mounts = parse_mountinfo("1 0 8:1 / / rw - ext4 /dev/sda1 rw");
        let aliases = derive_workspace_aliases("/home/admin", &mounts);
        assert_eq!(aliases, vec!["/home/admin".to_string()]);
    }

    #[test]
    fn aliases_detects_simple_bind_mount() {
        let mounts = parse_mountinfo(
            "\
1 0 8:1 / / rw - ext4 /dev/sda1 rw
10 1 8:1 /home/admin /elsewhere rw - ext4 /dev/sda1 rw
",
        );
        let aliases = derive_workspace_aliases("/home/admin", &mounts);
        assert!(aliases.contains(&"/home/admin".to_string()));
        assert!(aliases.contains(&"/elsewhere".to_string()));
    }

    #[test]
    fn aliases_detects_ancestor_bind_mount() {
        // mount --bind /home /mnt/home → workspace /home/admin becomes /mnt/home/admin
        let mounts = parse_mountinfo(
            "\
1 0 8:1 / / rw - ext4 /dev/sda1 rw
10 1 8:1 /home /mnt/home rw - ext4 /dev/sda1 rw
",
        );
        let matcher = WorkspaceCwdMatcher::new("/home/admin", &mounts);
        assert!(matcher.aliases.contains(&"/mnt/home/admin".to_string()));
        assert!(matcher.matches("/mnt/home/admin"));
        assert!(matcher.matches("/mnt/home/admin/x"));
        assert!(!matcher.matches("/mnt/home/administrator/x"));
    }

    #[test]
    fn aliases_separate_filesystem_mounted_workspace() {
        // Workspace on separate fs; bind-exposed at /backup.
        let mounts = parse_mountinfo(
            "\
1 0 8:1 / / rw - ext4 /dev/sda1 rw
10 1 8:32 / /data rw - ext4 /dev/sdb rw
20 1 8:32 / /backup rw - ext4 /dev/sdb rw
",
        );
        let aliases = derive_workspace_aliases("/data/ws", &mounts);
        assert!(aliases.contains(&"/backup/ws".to_string()));
    }

    #[test]
    fn aliases_ignores_different_device() {
        let mounts = parse_mountinfo(
            "\
1 0 8:1 / / rw - ext4 /dev/sda1 rw
10 1 8:99 /home/admin /decoy rw - ext4 /dev/sdg3 rw
",
        );
        let aliases = derive_workspace_aliases("/home/admin", &mounts);
        assert!(!aliases.contains(&"/decoy".to_string()));
    }

    #[test]
    fn aliases_detects_subpath_bind_mount() {
        let mounts = parse_mountinfo(
            "\
1 0 8:1 / / rw - ext4 /dev/sda1 rw
10 1 8:1 /data/ws/sub /mnt/sub rw - ext4 /dev/sda1 rw
11 1 8:1 /data/ws2 /mnt/sibling rw - ext4 /dev/sda1 rw
12 1 8:99 /data/ws/sub /mnt/other_dev rw - ext4 /dev/sdg3 rw
",
        );
        let matcher = WorkspaceCwdMatcher::new("/data/ws", &mounts);
        let aliases = derive_workspace_aliases("/data/ws", &mounts);
        assert!(aliases.contains(&"/data/ws".to_string()));
        assert!(aliases.contains(&"/mnt/sub".to_string()));
        assert!(!aliases.contains(&"/mnt/sibling".to_string()));
        assert!(!aliases.contains(&"/mnt/other_dev".to_string()));
        assert!(matcher.matches("/mnt/sub"));
        assert!(matcher.matches("/mnt/sub/deep/file"));
        assert!(!matcher.matches("/mnt/sub2"));
        assert!(!matcher.matches("/mnt/sibling"));
        assert!(!matcher.matches("/mnt/other_dev"));
    }

    #[test]
    fn aliases_detects_subpath_bind_mount_when_ws_is_fs_root() {
        let mounts = parse_mountinfo(
            "\
1 0 8:1 / / rw - ext4 /dev/sda1 rw
10 1 8:32 / /data rw - ext4 /dev/sdb rw
11 1 8:32 /home/sub /mnt/sub rw - ext4 /dev/sdb rw
",
        );
        let matcher = WorkspaceCwdMatcher::new("/data", &mounts);
        let aliases = derive_workspace_aliases("/data", &mounts);
        assert!(aliases.contains(&"/data".to_string()));
        assert!(aliases.contains(&"/mnt/sub".to_string()));
        assert!(matcher.matches("/mnt/sub"));
        assert!(matcher.matches("/mnt/sub/nested"));
        assert!(!matcher.matches("/mnt/sub2"));
    }

    #[test]
    fn stacked_overmount_hides_subpath_bind_mount() {
        // Exact overmount with a different device (tmpfs over /mnt/sub).
        let diff_dev_mounts = parse_mountinfo(
            "\
1 0 8:1 / / rw - ext4 /dev/sda1 rw
10 1 8:1 /data/ws/sub /mnt/sub rw - ext4 /dev/sda1 rw
20 10 0:50 / /mnt/sub rw - tmpfs tmpfs rw
",
        );
        let aliases = derive_workspace_aliases("/data/ws", &diff_dev_mounts);
        let matcher = WorkspaceCwdMatcher::new("/data/ws", &diff_dev_mounts);
        assert_eq!(aliases, vec!["/data/ws".to_string()]);
        assert!(!matcher.matches("/mnt/sub"));
        assert!(!matcher.matches("/mnt/sub/work"));
        assert!(matcher.matches("/data/ws/sub"));

        // Exact overmount with an unrelated directory on the same device.
        let same_dev_mounts = parse_mountinfo(
            "\
1 0 8:1 / / rw - ext4 /dev/sda1 rw
10 1 8:1 /data/ws/sub /mnt/sub rw - ext4 /dev/sda1 rw
20 10 8:1 /var/unrelated /mnt/sub rw - ext4 /dev/sda1 rw
",
        );
        let aliases = derive_workspace_aliases("/data/ws", &same_dev_mounts);
        let matcher = WorkspaceCwdMatcher::new("/data/ws", &same_dev_mounts);
        assert_eq!(aliases, vec!["/data/ws".to_string()]);
        assert!(!matcher.matches("/mnt/sub"));
        assert!(!matcher.matches("/mnt/sub/work"));

        // Ancestor overmount covering /mnt/sub.
        let ancestor_overmount = parse_mountinfo(
            "\
1 0 8:1 / / rw - ext4 /dev/sda1 rw
10 1 8:1 /data/ws/sub /mnt/sub rw - ext4 /dev/sda1 rw
20 1 0:50 / /mnt rw - tmpfs tmpfs rw
",
        );
        let aliases = derive_workspace_aliases("/data/ws", &ancestor_overmount);
        let matcher = WorkspaceCwdMatcher::new("/data/ws", &ancestor_overmount);
        assert_eq!(aliases, vec!["/data/ws".to_string()]);
        assert!(!matcher.matches("/mnt/sub"));
        assert!(!matcher.matches("/mnt/sub/work"));

        // Ancestor bind exposing /home/admin at /mnt/home/admin, with
        // /mnt/home/admin itself overmounted by an unrelated tmpfs.
        let overmounted_ancestor_alias = parse_mountinfo(
            "\
1 0 8:1 / / rw - ext4 /dev/sda1 rw
10 1 8:1 /home /mnt/home rw - ext4 /dev/sda1 rw
20 10 0:50 / /mnt/home/admin rw - tmpfs tmpfs rw
",
        );
        let aliases = derive_workspace_aliases("/home/admin", &overmounted_ancestor_alias);
        let matcher = WorkspaceCwdMatcher::new("/home/admin", &overmounted_ancestor_alias);
        assert_eq!(aliases, vec!["/home/admin".to_string()]);
        assert!(!matcher.matches("/mnt/home/admin"));
        assert!(!matcher.matches("/mnt/home/admin/work"));
    }

    #[test]
    fn nested_unrelated_mount_below_subpath_alias_is_not_occupant() {
        // Separate tmpfs mounted below a visible subpath bind mount, plus a
        // same-device unrelated bind mount and a deeper workspace re-bind.
        let mounts = parse_mountinfo(
            "\
1 0 8:1 / / rw - ext4 /dev/sda1 rw
10 1 8:1 /data/ws/sub /mnt/sub rw - ext4 /dev/sda1 rw
20 10 0:50 / /mnt/sub/cache rw - tmpfs tmpfs rw
21 10 8:1 /var/outside /mnt/sub/outside rw - ext4 /dev/sda1 rw
22 20 8:1 /data/ws/sub/deep /mnt/sub/cache/back rw - ext4 /dev/sda1 rw
",
        );
        let aliases = derive_workspace_aliases("/data/ws", &mounts);
        let matcher = WorkspaceCwdMatcher::new("/data/ws", &mounts);
        assert!(aliases.contains(&"/mnt/sub".to_string()));
        assert!(aliases.contains(&"/mnt/sub/cache/back".to_string()));

        // Visible subpath positive cases remain occupants.
        assert!(matcher.matches("/mnt/sub"));
        assert!(matcher.matches("/mnt/sub/src"));
        assert!(matcher.matches("/mnt/sub/cache2"));
        assert!(matcher.matches("/mnt/sub/cache/back"));
        assert!(matcher.matches("/mnt/sub/cache/back/file"));

        // Nested unrelated mounts below the alias are NOT workspace occupants.
        assert!(!matcher.matches("/mnt/sub/cache"));
        assert!(!matcher.matches("/mnt/sub/cache/work"));
        assert!(!matcher.matches("/mnt/sub/outside"));
        assert!(!matcher.matches("/mnt/sub/outside/work"));
    }

    #[test]
    fn mountinfo_child_before_parent_remains_visible() {
        // After switch_root / mount --move, a child mount can precede its
        // parent in /proc/self/mountinfo; mount_id/parent_id keeps it visible.
        let mounts = parse_mountinfo(
            "\
22 29 8:1 /data/ws/sub /mnt/sub rw - ext4 /dev/sda1 rw
29 0 8:1 / / rw - ext4 /dev/sda1 rw
",
        );
        let matcher = WorkspaceCwdMatcher::new("/data/ws", &mounts);
        assert!(matcher.matches("/mnt/sub"));
        assert!(matcher.matches("/mnt/sub/file"));
    }

    #[test]
    fn earlier_sibling_under_same_parent_does_not_hide_later_child() {
        // Only a *later* sibling overmount hides an earlier sibling under the
        // same parent_id; an earlier sibling at /mnt must not hide a later
        // mount at /mnt/sub.
        let mounts = parse_mountinfo(
            "\
1 0 8:1 / / rw - ext4 /dev/sda1 rw
10 1 0:50 / /mnt rw - tmpfs tmpfs rw
20 1 8:1 /data/ws/sub /mnt/sub rw - ext4 /dev/sda1 rw
",
        );
        let aliases = derive_workspace_aliases("/data/ws", &mounts);
        let matcher = WorkspaceCwdMatcher::new("/data/ws", &mounts);
        assert!(aliases.contains(&"/mnt/sub".to_string()));
        assert!(matcher.matches("/mnt/sub"));
        assert!(matcher.matches("/mnt/sub/file"));
        assert!(!matcher.matches("/mnt/other"));
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
