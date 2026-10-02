//! Local agent session file discovery.
//!
//! Scans known local AI agent session roots and returns JSONL files that can be
//! converted into ATIF. Qoder/QoderWork/Claude Code share the Claude-style JSONL
//! schema; Codex/Cursor roots are discovered here so converter support can be
//! added without changing the API surface.

use std::path::{Path, PathBuf};

/// Session file storage layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// `<root>/<project>/<session>.jsonl`, optionally with `transcript/` and
    /// `<session>/subagents/agent-*.jsonl` children.
    PerProject,
    /// `<root>/<session>.jsonl`.
    Flat,
}

/// Known local session roots relative to a home directory.
struct SessionRoot {
    rel_dir: &'static str,
    source: &'static str,
    layout: Layout,
    scan_subdirs: &'static [&'static str],
}

const SESSION_ROOTS: &[SessionRoot] = &[
    SessionRoot {
        rel_dir: ".claude/projects",
        source: "claude-code",
        layout: Layout::PerProject,
        scan_subdirs: &[],
    },
    SessionRoot {
        rel_dir: ".qoderwork/projects",
        source: "qoderwork",
        layout: Layout::PerProject,
        scan_subdirs: &["transcript"],
    },
    SessionRoot {
        rel_dir: ".qoder/projects",
        source: "qoder",
        layout: Layout::PerProject,
        scan_subdirs: &["transcript"],
    },
    SessionRoot {
        rel_dir: ".codex/sessions",
        source: "codex",
        layout: Layout::Flat,
        scan_subdirs: &[],
    },
    SessionRoot {
        rel_dir: ".codex/archived_sessions",
        source: "codex",
        layout: Layout::Flat,
        scan_subdirs: &[],
    },
    SessionRoot {
        rel_dir: ".cursor/projects",
        source: "cursor",
        layout: Layout::PerProject,
        scan_subdirs: &[],
    },
];

/// A discovered session file.
#[derive(Debug, Clone)]
pub struct DiscoveredSession {
    /// Path to the JSONL file.
    pub path: PathBuf,
    /// Decoded project name (e.g., "data-skillopt").
    pub project: String,
    /// Session identifier (filename UUID; subagents use
    /// `<parent>:subagent:<uuid>` to stay unique per file).
    pub session_id: String,
    /// Whether this is a sub-agent session.
    pub is_subagent: bool,
    /// Which product wrote the file: "qoder" or "qoderwork".
    pub source: String,
}

/// Discover all supported local sessions.
///
/// `scan_dirs` overrides the default scan roots. Each entry is interpreted by
/// path (`.qoderwork`, `.qoder`, `.claude`, `.codex`, `.cursor`) to choose the
/// source label and layout. When `None`, the current user's home directory is
/// probed for all known roots.
pub fn discover_sessions(scan_dirs: Option<&[PathBuf]>) -> Vec<DiscoveredSession> {
    let roots = match scan_dirs {
        Some(list) => list.iter().map(|dir| root_from_path(dir)).collect(),
        None => default_scan_roots(),
    };

    let mut sessions = Vec::new();
    for root in roots {
        if !root.dir.exists() {
            log::debug!(
                "Trajectory scan: skipping non-existent dir {}",
                root.dir.display()
            );
            continue;
        }
        sessions.extend(discover_in_root(&root));
    }

    // Sort by path for stable output
    sessions.sort_by(|a, b| a.path.cmp(&b.path));
    sessions
}

struct ScanRoot {
    dir: PathBuf,
    source: String,
    layout: Layout,
    scan_subdirs: &'static [&'static str],
}

/// Default session roots.
///
/// On Linux: scans `/root` + `/home/*` (all users, as before).
/// On non-Linux: scans only the current user's home directory.
fn default_scan_roots() -> Vec<ScanRoot> {
    let mut homes: Vec<PathBuf> = Vec::new();

    #[cfg(target_os = "linux")]
    {
        homes.push(PathBuf::from("/root"));
        if let Ok(entries) = std::fs::read_dir("/home") {
            for entry in entries.flatten() {
                homes.push(entry.path());
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        if let Some(home) = dirs::home_dir() {
            homes.push(home);
        }
    }

    let mut roots = Vec::new();
    for home in homes {
        for root in SESSION_ROOTS {
            let dir = home.join(root.rel_dir);
            if dir.exists() {
                roots.push(ScanRoot {
                    dir,
                    source: root.source.to_string(),
                    layout: root.layout,
                    scan_subdirs: root.scan_subdirs,
                });
            }
        }
    }
    roots
}

/// Infer the source and layout for a user-provided scan directory.
fn root_from_path(dir: &Path) -> ScanRoot {
    let path = dir.to_string_lossy().to_lowercase();
    let matched = SESSION_ROOTS
        .iter()
        .find(|root| path.contains(root.rel_dir));
    if let Some(root) = matched {
        return ScanRoot {
            dir: dir.to_path_buf(),
            source: root.source.to_string(),
            layout: root.layout,
            scan_subdirs: root.scan_subdirs,
        };
    }

    // Backward-compatible default: existing configs passed Qoder projects dirs.
    ScanRoot {
        dir: dir.to_path_buf(),
        source: source_from_path(dir),
        layout: if path.contains(".codex") {
            Layout::Flat
        } else {
            Layout::PerProject
        },
        scan_subdirs: if path.contains(".codex") {
            &[]
        } else {
            &["transcript"]
        },
    }
}

/// Derive the source label from a session root path.
fn source_from_path(dir: &Path) -> String {
    let path = dir.to_string_lossy().to_lowercase();
    if path.contains(".qoderwork") {
        "qoderwork".to_string()
    } else if path.contains(".qoder") {
        "qoder".to_string()
    } else if path.contains(".claude") {
        "claude-code".to_string()
    } else if path.contains(".codex") {
        "codex".to_string()
    } else if path.contains(".cursor") {
        "cursor".to_string()
    } else {
        "qoder".to_string()
    }
}

/// Discover sessions within a single configured root.
fn discover_in_root(root: &ScanRoot) -> Vec<DiscoveredSession> {
    match root.layout {
        Layout::PerProject => discover_in_projects_dir(root),
        Layout::Flat => discover_in_flat_dir(root),
    }
}

/// Discover sessions within a per-project root directory.
fn discover_in_projects_dir(root: &ScanRoot) -> Vec<DiscoveredSession> {
    let mut sessions = Vec::new();

    let entries = match std::fs::read_dir(&root.dir) {
        Ok(e) => e,
        Err(err) => {
            log::warn!(
                "Trajectory scan: failed to read {}: {err}",
                root.dir.display()
            );
            return sessions;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let project = decode_project_dir(&entry.file_name().to_string_lossy());
        sessions.extend(discover_in_project_dir(
            &path,
            &project,
            &root.source,
            root.scan_subdirs,
        ));
    }

    sessions
}

/// Discover sessions within a flat root directory.
fn discover_in_flat_dir(root: &ScanRoot) -> Vec<DiscoveredSession> {
    let mut sessions = Vec::new();
    discover_in_flat_dir_recursive(&root.dir, "(default)", &root.source, &mut sessions);
    sessions
}

fn discover_in_flat_dir_recursive(
    dir: &Path,
    project: &str,
    source: &str,
    sessions: &mut Vec<DiscoveredSession>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) => {
            log::debug!("Trajectory scan: failed to read {}: {err}", dir.display());
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            discover_in_flat_dir_recursive(&path, project, source, sessions);
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".jsonl") {
            continue;
        }
        let stem = name.trim_end_matches(".jsonl");
        if stem.is_empty() {
            continue;
        }
        sessions.push(DiscoveredSession {
            path,
            project: project.to_string(),
            session_id: stem.to_string(),
            is_subagent: false,
            source: source.to_string(),
        });
    }
}

/// Discover sessions within a single project directory.
fn discover_in_project_dir(
    project_dir: &Path,
    project: &str,
    source: &str,
    scan_subdirs: &[&str],
) -> Vec<DiscoveredSession> {
    let mut sessions = Vec::new();

    let entries = match std::fs::read_dir(project_dir) {
        Ok(e) => e,
        Err(err) => {
            log::debug!(
                "Trajectory scan: failed to read {}: {err}",
                project_dir.display()
            );
            return sessions;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);

        // Some agents store sessions under a whitelisted subdir such as
        // Qoder's `transcript/`; recurse one level so both layouts are covered.
        if is_dir && scan_subdirs.contains(&name.as_str()) {
            sessions.extend(discover_in_project_dir(
                &path,
                project,
                source,
                scan_subdirs,
            ));
            continue;
        }

        // Main session files: <uuid>.jsonl
        if !is_dir && name.ends_with(".jsonl") {
            let stem = name.trim_end_matches(".jsonl");
            // Skip agent-* files (sub-agents are stored under subagents/)
            if stem.starts_with("agent-") || !is_valid_session_id(stem) {
                continue;
            }
            sessions.push(DiscoveredSession {
                path: path.clone(),
                project: project.to_string(),
                session_id: stem.to_string(),
                is_subagent: false,
                source: source.to_string(),
            });
            continue;
        }

        // Sub-agent sessions: <uuid>/subagents/agent-<stem>.jsonl
        //
        // Historical Qoder versions named subagent files `agent-<uuid>.jsonl`;
        // newer versions embed the subagent type, e.g.
        // `agent-aExplore-b4b7e9141b9524f6.jsonl`. Accept any non-empty stem
        // (the `agent-` prefix + `.jsonl` suffix already gate the file).
        if is_dir && is_valid_session_id(&name) {
            let subagents_dir = path.join("subagents");
            let Ok(sub_entries) = std::fs::read_dir(&subagents_dir) else {
                continue;
            };
            for sub_entry in sub_entries.flatten() {
                let sub_name = sub_entry.file_name().to_string_lossy().to_string();
                if !sub_name.ends_with(".jsonl") || !sub_name.starts_with("agent-") {
                    continue;
                }
                let sub_stem = sub_name
                    .trim_end_matches(".jsonl")
                    .trim_start_matches("agent-");
                if is_valid_subagent_stem(sub_stem) {
                    sessions.push(DiscoveredSession {
                        path: sub_entry.path(),
                        project: project.to_string(),
                        session_id: format!("{name}:subagent:{sub_stem}"),
                        is_subagent: true,
                        source: source.to_string(),
                    });
                }
            }
        }
    }

    sessions
}

/// Decode a QoderWork/Qoder project directory name.
///
/// Examples:
/// - `-data-skillopt` → `data-skillopt`
/// - `-Users-john-projects-myapp` → `myapp`
/// - `myproject` → `myproject`
pub fn decode_project_dir(encoded: &str) -> String {
    if !encoded.starts_with('-') {
        return normalize_name(encoded);
    }

    let parts: Vec<&str> = encoded.split('-').filter(|s| !s.is_empty()).collect();

    // Look for known parent directory markers (use the LAST match)
    let parent_markers = [
        "code",
        "coding",
        "dev",
        "development",
        "projects",
        "repos",
        "src",
        "work",
        "workspace",
    ];

    let mut best_match: Option<usize> = None;
    for (i, part) in parts.iter().enumerate() {
        if parent_markers.contains(&part.to_lowercase().as_str()) {
            best_match = Some(i);
        }
    }

    if let Some(i) = best_match {
        if i + 1 < parts.len() {
            let project = parts[i + 1..].join("-");
            if !project.is_empty() {
                return normalize_name(&project);
            }
        }
    }

    // No marker found — return all parts joined (just strip leading dash)
    normalize_name(&parts.join("-"))
}

/// Simple name normalization.
fn normalize_name(s: &str) -> String {
    s.trim_start_matches(['-', '.']).to_string()
}

/// Check if a string looks like a valid session ID (UUID format).
fn is_valid_session_id(s: &str) -> bool {
    // UUID format: 8-4-4-4-12 hex digits
    if s.len() != 36 {
        return false;
    }
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 5 {
        return false;
    }
    let expected_lens = [8, 4, 4, 4, 12];
    for (part, &expected_len) in parts.iter().zip(expected_lens.iter()) {
        if part.len() != expected_len || !part.chars().all(|c| c.is_ascii_hexdigit()) {
            return false;
        }
    }
    true
}

/// Check if a subagent file stem is usable as a session-id component.
///
/// Accepts both the legacy UUID form and the newer `<type>-<hex>` form
/// (e.g. `aExplore-b4b7e9141b9524f6`). Only rejects empty stems and stems
/// with characters that would be unsafe in a composite session id.
fn is_valid_subagent_stem(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID_A: &str = "be0aa488-4e56-4604-bdf0-e12cc387392d";
    const UUID_B: &str = "11111111-2222-3333-4444-555555555555";

    fn tmp_projects_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("traj-disc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_decode_project_dir() {
        assert_eq!(decode_project_dir("-data-skillopt"), "data-skillopt");
        assert_eq!(decode_project_dir("-Users-john-projects-myapp"), "myapp");
        assert_eq!(decode_project_dir("myproject"), "myproject");
        assert_eq!(decode_project_dir("-data-work-repos-foo"), "foo");
    }

    #[test]
    fn test_is_valid_session_id() {
        assert!(is_valid_session_id(UUID_A));
        assert!(!is_valid_session_id("agent-123"));
        assert!(!is_valid_session_id("not-a-uuid"));
        assert!(!is_valid_session_id(""));
    }

    #[test]
    fn test_is_valid_subagent_stem() {
        // Legacy UUID form
        assert!(is_valid_subagent_stem(UUID_B));
        // Newer <type>-<hex> form
        assert!(is_valid_subagent_stem("aExplore-b4b7e9141b9524f6"));
        assert!(is_valid_subagent_stem("ageneral-purpose-fa6bcbf451087ee5"));
        // Rejects empty and unsafe characters
        assert!(!is_valid_subagent_stem(""));
        assert!(!is_valid_subagent_stem("bad/name"));
        assert!(!is_valid_subagent_stem("has space"));
    }

    #[test]
    fn test_discover_filters_and_subagents() {
        let projects = tmp_projects_dir("main");
        let proj = projects.join("-data-myapp");
        std::fs::create_dir_all(&proj).unwrap();

        // Valid main session
        std::fs::write(proj.join(format!("{UUID_A}.jsonl")), "{}\n").unwrap();
        // Invalid names must be skipped
        std::fs::write(proj.join("notes.jsonl"), "{}\n").unwrap();
        std::fs::write(proj.join(format!("agent-{UUID_B}.jsonl")), "{}\n").unwrap();
        // Sub-agent session under <uuid>/subagents/ (legacy UUID name)
        let sub = proj.join(UUID_A).join("subagents");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(format!("agent-{UUID_B}.jsonl")), "{}\n").unwrap();
        // Newer <type>-<hex> subagent name must also be discovered
        std::fs::write(sub.join("agent-aExplore-b4b7e9141b9524f6.jsonl"), "{}\n").unwrap();

        let sessions = discover_sessions(Some(std::slice::from_ref(&projects)));
        assert_eq!(sessions.len(), 3, "one main + two subagents: {sessions:?}");

        let main = sessions.iter().find(|s| !s.is_subagent).unwrap();
        assert_eq!(main.session_id, UUID_A);
        assert_eq!(main.project, "data-myapp");
        assert_eq!(main.source, "qoder");

        assert!(sessions
            .iter()
            .any(|s| s.is_subagent && s.session_id == format!("{UUID_A}:subagent:{UUID_B}")));
        assert!(sessions.iter().any(|s| s.is_subagent
            && s.session_id == format!("{UUID_A}:subagent:aExplore-b4b7e9141b9524f6")));

        let _ = std::fs::remove_dir_all(&projects);
    }

    #[test]
    fn test_discover_transcript_subdir_layout() {
        let projects = tmp_projects_dir("transcript");
        let proj = projects.join("-data-myapp");
        // Newer Qoder layout: sessions under <project>/transcript/
        let transcript = proj.join("transcript");
        std::fs::create_dir_all(&transcript).unwrap();
        std::fs::write(transcript.join(format!("{UUID_A}.jsonl")), "{}\n").unwrap();
        // Old layout in the same project still works
        std::fs::write(proj.join(format!("{UUID_B}.jsonl")), "{}\n").unwrap();

        let sessions = discover_sessions(Some(std::slice::from_ref(&projects)));
        assert_eq!(sessions.len(), 2, "transcript + root layout: {sessions:?}");
        assert!(sessions
            .iter()
            .any(|s| s.session_id == UUID_A && s.path.to_string_lossy().contains("/transcript/")));
        assert!(sessions.iter().any(|s| s.session_id == UUID_B));

        let _ = std::fs::remove_dir_all(&projects);
    }

    #[test]
    fn test_discover_flat_recursive_layout() {
        let base = tmp_projects_dir("flat");
        let root = base.join(".codex").join("sessions");
        let nested = root.join("2026").join("07").join("27");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("rollout-abc123.jsonl"), "{}\n").unwrap();

        let sessions = discover_sessions(Some(std::slice::from_ref(&root)));
        assert_eq!(sessions.len(), 1, "recursive flat layout: {sessions:?}");
        assert_eq!(sessions[0].session_id, "rollout-abc123");
        assert_eq!(sessions[0].source, "codex");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_discover_known_claude_root_source() {
        let base = tmp_projects_dir("claude");
        let root = base.join(".claude").join("projects");
        let proj = root.join("-Users-alice-projects-demo");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join(format!("{UUID_A}.jsonl")), "{}\n").unwrap();

        let sessions = discover_sessions(Some(std::slice::from_ref(&root)));
        assert_eq!(sessions.len(), 1, "claude root: {sessions:?}");
        assert_eq!(sessions[0].source, "claude-code");
        assert_eq!(sessions[0].project, "demo");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_source_from_path() {
        assert_eq!(
            source_from_path(Path::new("/root/.qoderwork/projects")),
            "qoderwork"
        );
        assert_eq!(
            source_from_path(Path::new("/home/u/.qoder/projects")),
            "qoder"
        );
    }

    #[test]
    fn test_default_scan_roots_enumerates_linux_homes() {
        // On Linux, default_scan_roots() must include /root and all /home/*
        // users (multi-user collection). On other platforms it uses the
        // current user's home directory only.
        let roots = default_scan_roots();

        #[cfg(target_os = "linux")]
        {
            let mut homes = vec![PathBuf::from("/root")];
            if let Ok(entries) = std::fs::read_dir("/home") {
                for entry in entries.flatten() {
                    homes.push(entry.path());
                }
            }
            let root_dirs: Vec<_> = roots.iter().map(|r| &r.dir).collect();
            for home in &homes {
                if !home.exists() {
                    continue;
                }
                // Only assert if this home actually has session dirs;
                // not every /home/* user will have .claude/.qoder etc.
                let has_session_dir = SESSION_ROOTS.iter().any(|r| home.join(r.rel_dir).exists());
                if has_session_dir {
                    assert!(
                        root_dirs.iter().any(|d| d.starts_with(home)),
                        "scan roots should include entries under {}",
                        home.display()
                    );
                }
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            let home = dirs::home_dir().expect("home_dir should be available");
            for root in &roots {
                assert!(
                    root.dir.starts_with(&home),
                    "scan root {} should be under home {}",
                    root.dir.display(),
                    home.display(),
                );
            }
        }

        // discover_sessions(None) should not panic and should return a Vec.
        let sessions = discover_sessions(None);

        #[cfg(not(target_os = "linux"))]
        {
            let home = dirs::home_dir().expect("home_dir should be available");
            for session in &sessions {
                assert!(
                    session.path.starts_with(&home),
                    "discovered session {} should be under home {}",
                    session.path.display(),
                    home.display(),
                );
            }
        }

        #[cfg(target_os = "linux")]
        {
            let _ = sessions; // just ensure it doesn't panic
        }
    }
}
