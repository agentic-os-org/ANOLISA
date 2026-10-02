//! Integration tests for the 10 Tier A file tools.
//!
//! Each tool gets at least one happy-path case and one error-path case
//! (path-sandbox, missing file, etc.).

use tempfile::tempdir;

use agent_memory::config::{AppConfig, Profile};
use agent_memory::error::MemoryError;
use agent_memory::service::MemoryService;
use agent_memory::tools::{GrepOptions, ListOptions};

fn setup() -> (tempfile::TempDir, MemoryService) {
    let tmp = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "tester".into();
    cfg.memory.profile = Profile::Advanced;
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    let svc = MemoryService::new(cfg).unwrap();
    (tmp, svc)
}

fn setup_with_read_cap(cap: u64) -> (tempfile::TempDir, MemoryService) {
    let tmp = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "tester".into();
    cfg.memory.profile = Profile::Advanced;
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    cfg.memory.max_read_bytes = cap;
    let svc = MemoryService::new(cfg).unwrap();
    (tmp, svc)
}

// ---------- mem_write / mem_read ----------

#[test]
fn write_then_read() {
    let (_t, svc) = setup();
    svc.write("notes/foo.md", "hello world", false).unwrap();
    let body = svc.read("notes/foo.md").unwrap();
    assert_eq!(body, "hello world");
}

#[test]
fn read_missing_file_returns_not_found() {
    let (_t, svc) = setup();
    let err = svc.read("nope.md").unwrap_err();
    assert!(matches!(err, MemoryError::NotFound(_)), "got: {err:?}");
}

#[test]
fn read_rejects_file_exceeding_cap() {
    let (tmp, svc) = setup_with_read_cap(10);
    svc.write("big.md", "this content is way more than ten bytes", false)
        .unwrap();
    let err = svc.read("big.md").unwrap_err();
    assert!(matches!(err, MemoryError::InvalidArgument(_)));
    assert!(
        err.to_string().contains("exceeds read limit"),
        "expected read limit error, got: {err}"
    );
    // File still exists on disk — cap only blocks the read, not the write.
    assert!(tmp.path().join("user-tester").join("big.md").exists());
}

#[test]
fn read_allows_file_within_cap() {
    let (_t, svc) = setup_with_read_cap(100);
    svc.write("small.md", "hello", false).unwrap();
    let body = svc.read("small.md").unwrap();
    assert_eq!(body, "hello");
}

#[test]
fn write_then_overwrite_requires_flag() {
    let (_t, svc) = setup();
    svc.write("a.md", "v1", false).unwrap();
    let err = svc.write("a.md", "v2", false).unwrap_err();
    assert!(matches!(err, MemoryError::AlreadyExists(_)));
    svc.write("a.md", "v2", true).unwrap();
    assert_eq!(svc.read("a.md").unwrap(), "v2");
}

// ---------- path sandbox ----------

#[test]
fn rejects_parent_dir_escape() {
    let (_t, svc) = setup();
    let err = svc.read("../../etc/passwd").unwrap_err();
    assert!(
        matches!(err, MemoryError::PathOutsideMount(_)),
        "got: {err:?}"
    );
}

#[test]
fn rejects_absolute_path() {
    let (_t, svc) = setup();
    let err = svc.write("/tmp/escape", "x", false).unwrap_err();
    assert!(matches!(err, MemoryError::PathOutsideMount(_)));
}

#[test]
fn rejects_reserved_segment_access() {
    let (_t, svc) = setup();
    let err = svc.write(".anolisa/audit.log", "x", true).unwrap_err();
    assert!(matches!(err, MemoryError::TargetIsReserved(_)));
    let err = svc.read(".anolisa/audit.log").unwrap_err();
    assert!(matches!(err, MemoryError::TargetIsReserved(_)));
    // .gitignore and .git are also reserved.
    let err = svc.write(".gitignore", "", true).unwrap_err();
    assert!(matches!(err, MemoryError::TargetIsReserved(_)));
    let err = svc.write(".git/config", "x", true).unwrap_err();
    assert!(matches!(err, MemoryError::TargetIsReserved(_)));
}

// ---------- mem_append ----------

#[test]
fn append_creates_and_appends() {
    let (_t, svc) = setup();
    svc.append("log.txt", "line1\n").unwrap();
    svc.append("log.txt", "line2\n").unwrap();
    assert_eq!(svc.read("log.txt").unwrap(), "line1\nline2\n");
}

// ---------- mem_edit ----------

#[test]
fn edit_replaces_unique_occurrence() {
    let (_t, svc) = setup();
    svc.write("doc.md", "title: foo\nbody: hello", false)
        .unwrap();
    svc.edit("doc.md", "hello", "world").unwrap();
    assert_eq!(svc.read("doc.md").unwrap(), "title: foo\nbody: world");
}

#[test]
fn edit_rejects_zero_or_multi_match() {
    let (_t, svc) = setup();
    svc.write("doc.md", "abc abc abc", false).unwrap();
    let err = svc.edit("doc.md", "abc", "x").unwrap_err();
    assert!(
        matches!(err, MemoryError::InvalidArgument(ref m) if m.contains("multiple occurrences"))
    );

    let err = svc.edit("doc.md", "missing", "x").unwrap_err();
    assert!(matches!(err, MemoryError::InvalidArgument(ref m) if m.contains("not found")));
}

// ---------- mem_mkdir / mem_remove ----------

#[test]
fn mkdir_idempotent() {
    let (_t, svc) = setup();
    svc.mkdir("a/b/c").unwrap();
    svc.mkdir("a/b/c").unwrap();
}

#[test]
fn remove_file_then_dir() {
    let (_t, svc) = setup();
    svc.write("d/x.md", "x", false).unwrap();
    svc.remove("d/x.md", false).unwrap();
    let err = svc.read("d/x.md").unwrap_err();
    assert!(matches!(err, MemoryError::NotFound(_)));

    // dir is non-empty after recreating; recursive=false rejects
    svc.write("d/y.md", "y", false).unwrap();
    let err = svc.remove("d", false).unwrap_err();
    assert!(matches!(err, MemoryError::InvalidArgument(_)));
    svc.remove("d", true).unwrap();
}

// ---------- mem_list ----------

#[test]
fn list_root_includes_readme() {
    let (_t, svc) = setup();
    let entries = svc.list("", ListOptions::default()).unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    assert!(names.contains(&"README.md"), "got: {names:?}");
}

#[test]
fn list_recursive_with_glob() {
    let (_t, svc) = setup();
    svc.write("notes/a.md", "a", false).unwrap();
    svc.write("notes/b.md", "b", false).unwrap();
    svc.write("notes/c.txt", "c", false).unwrap();

    let opts = ListOptions {
        recursive: true,
        glob: Some("**/*.md".into()),
    };
    let entries = svc.list("", opts).unwrap();
    let mds: Vec<&str> = entries
        .iter()
        .filter(|e| e.path.ends_with(".md"))
        .map(|e| e.path.as_str())
        .collect();
    assert!(mds.contains(&"notes/a.md"));
    assert!(mds.contains(&"notes/b.md"));
    // c.txt should NOT pass the glob filter
    assert!(!entries.iter().any(|e| e.path == "notes/c.txt"));
}

#[test]
fn list_hides_meta_dir() {
    let (_t, svc) = setup();
    let entries = svc
        .list(
            "",
            ListOptions {
                recursive: true,
                glob: None,
            },
        )
        .unwrap();
    assert!(!entries.iter().any(|e| e.path.starts_with(".anolisa")));
}

// ---------- mem_grep ----------

#[test]
fn grep_finds_matches_with_line_numbers() {
    let (_t, svc) = setup();
    svc.write(
        "notes/a.md",
        "first line\nsecond hello world\nthird line\n",
        false,
    )
    .unwrap();
    svc.write("notes/b.md", "no match here", false).unwrap();

    let opts = GrepOptions::default();
    let hits = svc.grep("hello", opts).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "notes/a.md");
    assert_eq!(hits[0].line, 2);
    assert!(hits[0].text.contains("hello"));
}

#[test]
fn grep_respects_case_insensitive() {
    let (_t, svc) = setup();
    svc.write("a.md", "Hello World", false).unwrap();

    let hits = svc.grep("hello", GrepOptions::default()).unwrap();
    assert_eq!(hits.len(), 0);

    let hits = svc
        .grep(
            "hello",
            GrepOptions {
                case_insensitive: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn grep_respects_glob_filter() {
    let (_t, svc) = setup();
    svc.write("notes/a.md", "match", false).unwrap();
    svc.write("logs/a.txt", "match", false).unwrap();

    let hits = svc
        .grep(
            "match",
            GrepOptions {
                r#type: Some("notes/**".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "notes/a.md");
}

// ---------- mem_diff ----------

#[test]
fn diff_shows_changes() {
    let (_t, svc) = setup();
    svc.write("a.md", "line1\nline2\n", false).unwrap();
    svc.write("b.md", "line1\nline2 changed\n", false).unwrap();
    let patch = svc.diff("a.md", "b.md").unwrap();
    assert!(patch.contains("--- a.md"));
    assert!(patch.contains("+++ b.md"));
    assert!(patch.contains("line2 changed"));
}

#[test]
fn diff_missing_file_errors() {
    let (_t, svc) = setup();
    svc.write("a.md", "x", false).unwrap();
    let err = svc.diff("a.md", "nope.md").unwrap_err();
    assert!(matches!(err, MemoryError::NotFound(_)));
}

// mem_promote happy / error paths are exercised in tests/session_test.rs
// (promote_copies_scratch_to_store, promote_missing_scratch_file_returns_not_found,
//  session_log_degrades_gracefully_when_session_dir_unavailable).

// ---------- audit log smoke ----------

#[test]
fn audit_log_records_operations() {
    let (_t, svc) = setup();
    svc.write("notes/a.md", "x", false).unwrap();
    svc.read("notes/a.md").unwrap();
    let _ = svc.read("missing.md");

    let log = std::fs::read_to_string(svc.mount.audit_log_path()).unwrap();
    let lines: Vec<&str> = log.lines().filter(|l| !l.is_empty()).collect();
    assert!(
        lines.len() >= 3,
        "expected ≥3 audit lines, got {}",
        lines.len()
    );

    // Each line must be valid JSON with required fields
    for l in &lines {
        let v: serde_json::Value = serde_json::from_str(l).expect("not JSON");
        assert!(v["ts"].is_string());
        assert!(v["tool"].is_string());
        assert!(v["ok"].is_boolean());
    }
}

// ---------- chinese filename support ----------

#[test]
fn supports_chinese_paths() {
    let (_t, svc) = setup();
    svc.write("笔记/想法.md", "你好世界", false).unwrap();
    let body = svc.read("笔记/想法.md").unwrap();
    assert_eq!(body, "你好世界");

    let hits = svc.grep("你好", GrepOptions::default()).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "笔记/想法.md");
}

// ---------- symlink TOCTOU sandboxing (B2 regression) ----------

#[cfg(target_os = "linux")]
mod symlink_attacks {
    use super::*;
    use std::os::unix::fs::symlink;

    /// Plant a symlink under the mount root and verify each tool refuses
    /// to traverse it. The targets in `outside` represent the attacker's
    /// goal (read secrets, write to /etc, remove user dirs). Pre-fix all
    /// of these would have succeeded; post-fix every one returns
    /// PathOutsideMount via the openat2 BENEATH/NO_SYMLINKS guard.
    fn link_into_mount(svc: &MemoryService, link_rel: &str, target_abs: &std::path::Path) {
        let link = svc.mount.root.join(link_rel);
        if let Some(parent) = link.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        symlink(target_abs, link).unwrap();
    }

    #[test]
    fn read_refuses_symlink_to_outside_file() {
        let (_t, svc) = setup();
        let outside = tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "TOP_SECRET").unwrap();
        link_into_mount(&svc, "notes/leak", &secret);

        let err = svc.read("notes/leak").unwrap_err();
        assert!(
            matches!(err, MemoryError::PathOutsideMount(_)),
            "got {err:?}"
        );
    }

    #[test]
    fn write_refuses_symlink_target() {
        let (_t, svc) = setup();
        let outside = tempdir().unwrap();
        let victim = outside.path().join("victim.txt");
        std::fs::write(&victim, "original").unwrap();
        link_into_mount(&svc, "notes/clobber", &victim);

        let err = svc.write("notes/clobber", "PWNED", true).unwrap_err();
        assert!(matches!(err, MemoryError::PathOutsideMount(_)));
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "original");
    }

    #[test]
    fn write_refuses_symlink_parent_dir() {
        let (_t, svc) = setup();
        let outside = tempdir().unwrap();
        std::fs::create_dir(outside.path().join("victimdir")).unwrap();
        link_into_mount(&svc, "notes", outside.path().join("victimdir").as_path());

        let err = svc.write("notes/escape.md", "evil", false).unwrap_err();
        assert!(matches!(err, MemoryError::PathOutsideMount(_)));
        assert!(!outside.path().join("victimdir/escape.md").exists());
    }

    #[test]
    fn remove_refuses_symlink_target() {
        let (_t, svc) = setup();
        let outside = tempdir().unwrap();
        let victim = outside.path().join("important.txt");
        std::fs::write(&victim, "do-not-delete").unwrap();
        link_into_mount(&svc, "trap", &victim);

        let err = svc.remove("trap", false).unwrap_err();
        assert!(matches!(err, MemoryError::PathOutsideMount(_)));
        assert!(victim.exists(), "victim file should still exist");
    }

    #[test]
    fn remove_refuses_symlink_to_outside_dir() {
        let (_t, svc) = setup();
        let outside = tempdir().unwrap();
        let victim_dir = outside.path().join("victim");
        std::fs::create_dir(&victim_dir).unwrap();
        std::fs::write(victim_dir.join("important.md"), "x").unwrap();
        link_into_mount(&svc, "trap_dir", &victim_dir);

        let err = svc.remove("trap_dir", true).unwrap_err();
        assert!(matches!(err, MemoryError::PathOutsideMount(_)));
        assert!(victim_dir.join("important.md").exists());
    }

    #[test]
    fn mkdir_refuses_inside_symlinked_dir() {
        let (_t, svc) = setup();
        let outside = tempdir().unwrap();
        std::fs::create_dir(outside.path().join("escape")).unwrap();
        link_into_mount(&svc, "out", outside.path().join("escape").as_path());

        let err = svc.mkdir("out/inside").unwrap_err();
        assert!(matches!(err, MemoryError::PathOutsideMount(_)));
        assert!(!outside.path().join("escape/inside").exists());
    }
}
