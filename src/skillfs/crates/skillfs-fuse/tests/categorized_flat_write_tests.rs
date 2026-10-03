//! Regression coverage for flat FUSE views backed by categorized sources.

use std::ffi::CString;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::{MountConfig, MountOptions, mount_background_configured};

mod common;

fn wait_for(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    predicate()
}

fn access(path: &Path, mask: i32) -> i32 {
    let path = CString::new(path.as_os_str().as_bytes()).expect("path CString");
    let result = unsafe { libc::access(path.as_ptr(), mask) };
    if result == 0 {
        0
    } else {
        std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO)
    }
}

#[cfg(target_os = "linux")]
fn renameat2(old: &Path, new: &Path, flags: u32) -> Result<(), i32> {
    let old = CString::new(old.as_os_str().as_bytes()).expect("old path CString");
    let new = CString::new(new.as_os_str().as_bytes()).expect("new path CString");
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            old.as_ptr(),
            libc::AT_FDCWD,
            new.as_ptr(),
            flags,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO))
    }
}

#[test]
fn categorized_source_supports_flat_reads_writes_and_sync() {
    skip_if_no_fuse!();

    let source = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");
    let skill_dir = source.path().join("catalog/demo");
    std::fs::create_dir_all(&skill_dir).expect("categorized skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: demo\ndescription: categorized\n---\noriginal body\n",
    )
    .expect("categorized SKILL.md");
    std::fs::write(skill_dir.join("notes.txt"), "original notes").expect("categorized passthrough");

    let mut initial_store = SkillStore::new();
    initial_store.load_from_directory(source.path(), &ParseConfig::default());
    let store: SharedSkillStore = Arc::new(RwLock::new(initial_store));
    assert_eq!(
        store.read().get("demo").expect("loaded demo").source_path,
        skill_dir.join("SKILL.md")
    );

    let handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        store.clone(),
        MountOptions::default(),
        false,
        MountConfig::default(),
    )
    .expect("mount categorized source");
    std::thread::sleep(Duration::from_millis(300));

    let virtual_skill = mountpoint.path().join("skills/demo");
    let virtual_md = virtual_skill.join("SKILL.md");
    let virtual_notes = virtual_skill.join("notes.txt");
    let compiled = std::fs::read_to_string(&virtual_md).expect("compiled categorized read");
    assert!(compiled.contains("original body"));
    assert_eq!(access(&virtual_md, libc::W_OK), 0);
    assert_eq!(access(&virtual_notes, libc::W_OK), 0);

    let mut md = std::fs::OpenOptions::new()
        .append(true)
        .open(&virtual_md)
        .expect("append categorized SKILL.md");
    md.write_all(b"appended through flat mount\n")
        .expect("write categorized SKILL.md");
    drop(md);

    let mut notes = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&virtual_notes)
        .expect("open categorized passthrough");
    notes
        .write_all(b"updated through flat mount")
        .expect("write categorized passthrough");
    drop(notes);

    assert!(
        wait_for(Duration::from_secs(3), || {
            store
                .read()
                .get("demo")
                .is_some_and(|entry| entry.body.contains("appended through flat mount"))
        }),
        "sync worker did not refresh the categorized store entry"
    );
    assert!(
        std::fs::read_to_string(skill_dir.join("SKILL.md"))
            .expect("physical categorized SKILL.md")
            .contains("appended through flat mount")
    );
    assert_eq!(
        std::fs::read_to_string(skill_dir.join("notes.txt"))
            .expect("physical categorized passthrough"),
        "updated through flat mount"
    );
    assert!(
        std::fs::read_to_string(&virtual_md)
            .expect("compiled read after sync")
            .contains("appended through flat mount")
    );

    let virtual_link = virtual_skill.join("notes-link.txt");
    std::fs::hard_link(&virtual_notes, &virtual_link)
        .expect("hardlink categorized passthrough through flat mount");
    let physical_notes = skill_dir.join("notes.txt");
    let physical_link = skill_dir.join("notes-link.txt");
    assert_eq!(
        std::fs::read_to_string(&physical_link).expect("physical categorized hardlink"),
        "updated through flat mount"
    );
    assert_eq!(
        std::fs::metadata(&physical_notes)
            .expect("physical categorized source metadata")
            .ino(),
        std::fs::metadata(&physical_link)
            .expect("physical categorized hardlink metadata")
            .ino(),
        "categorized hardlink must share the source inode"
    );
    assert!(
        !source.path().join("demo").exists(),
        "flat mutations must not create an incorrect source/demo directory"
    );

    let candidate_dir = source.path().join("reserved-demo");
    std::fs::create_dir(&candidate_dir).expect("source-root candidate dir");
    std::fs::write(candidate_dir.join("payload.txt"), "candidate payload")
        .expect("source-root candidate payload");
    let candidate_virtual = mountpoint.path().join("skills/reserved-demo");

    #[cfg(target_os = "linux")]
    assert_eq!(
        renameat2(&virtual_skill, &candidate_virtual, libc::RENAME_NOREPLACE),
        Err(libc::EEXIST),
        "NOREPLACE must preserve an existing source-root candidate"
    );

    let rename_error = std::fs::rename(&virtual_skill, &candidate_virtual)
        .expect_err("plain rename must not ignore a non-empty source-root candidate");
    assert!(
        matches!(
            rename_error.raw_os_error(),
            Some(error) if error == libc::EEXIST || error == libc::ENOTEMPTY
        ),
        "plain rename returned an unexpected error: {rename_error}"
    );
    assert!(
        skill_dir.is_dir(),
        "categorized source must remain after conflict"
    );
    assert_eq!(
        std::fs::read_to_string(candidate_dir.join("payload.txt"))
            .expect("source-root candidate remains readable"),
        "candidate payload"
    );
    assert!(
        !source.path().join("catalog/reserved-demo").exists(),
        "conflicting rename must not create a categorized sibling"
    );
    assert!(store.read().get("demo").is_some());
    assert!(store.read().get("reserved-demo").is_none());
    std::fs::remove_dir_all(&candidate_dir).expect("remove source-root candidate");

    let renamed_virtual = mountpoint.path().join("skills/renamed-demo");
    std::fs::rename(&virtual_skill, &renamed_virtual).expect("rename categorized flat skill");
    assert!(source.path().join("catalog/renamed-demo").is_dir());
    assert!(!source.path().join("renamed-demo").exists());
    assert!(
        std::fs::read_to_string(renamed_virtual.join("SKILL.md"))
            .expect("compiled read after categorized rename")
            .contains("appended through flat mount")
    );
    let renamed_entry_path = store
        .read()
        .get("renamed-demo")
        .expect("renamed store entry")
        .source_path
        .clone();
    assert_eq!(
        renamed_entry_path,
        source.path().join("catalog/renamed-demo/SKILL.md")
    );

    handle.unmount().expect("unmount categorized source");
}
