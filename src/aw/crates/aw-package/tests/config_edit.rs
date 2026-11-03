//! Offline editing, idempotency and failure-preserving publication contracts.

use aw_package::config_edit::ConfigurationEditor;
use serde_json::{json, Value};
use std::{
    fs,
    os::{
        fd::AsRawFd,
        unix::fs::{chown, symlink, MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

const MINIMAL: &str = include_str!("../../aw-config/examples/aw.minimal.yaml");

struct Fixture {
    directory: tempfile::TempDir,
    path: PathBuf,
}

impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/config-edit");
        fs::create_dir_all(&target).unwrap();
        let directory = tempfile::tempdir_in(target.canonicalize().unwrap()).unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.path().join("aw.yaml");
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        Self { directory, path }
    }

    fn editor(&self) -> ConfigurationEditor {
        ConfigurationEditor::open(&self.path).unwrap()
    }

    fn bytes(&self) -> Vec<u8> {
        fs::read(&self.path).unwrap()
    }

    fn only_configuration_remains(&self) {
        assert_eq!(fs::read_dir(self.directory.path()).unwrap().count(), 1);
    }
}

fn provider(native: bool) -> Value {
    json!({
        "protocol":if native { "native-hook/v1alpha1" } else { "aw-provider/v1alpha1" },
        "transport":{"type":"stdio","location":"agent","argv":["/unexecuted/hook"]},
        "timeout_ms":1000,"max_output_bytes":4096,
        "config":if native { json!({}) } else { json!({"未知字段":{"ratio":0.125,"list":[false,null,7]},"password":"private-value"}) }
    })
}

fn hook(id: &str, provider: &str, native: bool) -> Value {
    if native {
        json!({"id":id,"provider":provider,"native":{},"on_error":"report"})
    } else {
        json!({"id":id,"provider":provider,"operation":"observe","effects":["observe"],"on_error":"report"})
    }
}

#[test]
fn structured_and_native_hooks_preserve_unrelated_values_options_and_order() {
    let fixture = Fixture::new(MINIMAL.as_bytes());
    let mut editor = fixture.editor();
    let original = editor.as_value().clone();
    editor.add_provider("policy", provider(false)).unwrap();
    editor.add_provider("native", provider(true)).unwrap();
    for (id, provider, native) in [("first", "policy", false), ("second", "native", true)] {
        assert!(editor
            .add_hook("tool.before", hook(id, provider, native))
            .unwrap());
    }
    // A Hook ID belongs to its event, rather than a global namespace.
    editor
        .add_hook("tool.after", hook("first", "policy", false))
        .unwrap();
    editor.validate().unwrap();
    assert_eq!(editor.provider("policy"), Some(&provider(false)));
    assert_eq!(
        editor.hook("tool.before", "second"),
        Some(&hook("second", "native", true))
    );
    assert!(editor.provider("missing").is_none());
    assert!(editor.hook("absent", "first").is_none());
    assert!(editor.save().unwrap());
    let saved = fixture.editor();
    assert_eq!(saved.as_value(), editor.as_value());
    for field in ["daemon", "execution", "audit", "agents"] {
        assert_eq!(saved.as_value()["spec"][field], original["spec"][field]);
    }
    assert_eq!(
        saved.as_value()["spec"]["events"]["tool.before"]["required"],
        true
    );
    assert_eq!(
        saved.as_value()["spec"]["events"]["tool.before"]["steps"][0]["id"],
        "first"
    );
    assert_eq!(
        saved.as_value()["spec"]["events"]["tool.before"]["steps"][1]["id"],
        "second"
    );
    assert_eq!(fs::metadata(&fixture.path).unwrap().mode() & 0o777, 0o600);
    // Saving twice and editing the newly published inode both remain supported.
    assert!(!editor.save().unwrap());
    editor.remove_hook("tool.before", "second").unwrap();
    editor.remove_provider("native").unwrap();
    assert!(editor.save().unwrap());
    fixture.only_configuration_remains();
}

#[test]
fn replacement_preserves_group_read_access_and_rejects_changed_groups() {
    let fixture = Fixture::new(MINIMAL.as_bytes());
    let original_group = fs::metadata(&fixture.path).unwrap().gid();
    // SAFETY: the first call queries the length; the second uses that-sized storage.
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    assert!(count >= 0);
    let mut groups = vec![0; count as usize];
    assert_eq!(
        unsafe { libc::getgroups(count, groups.as_mut_ptr()) },
        count
    );
    let Some(group) = groups
        .into_iter()
        .find(|group| *group != original_group)
        .or_else(|| (fs::metadata(&fixture.path).unwrap().uid() == 0).then_some(1))
    else {
        eprintln!("group-preservation test requires root or a supplementary group");
        return;
    };
    chown(&fixture.path, None, Some(group)).unwrap();
    fs::set_permissions(&fixture.path, fs::Permissions::from_mode(0o640)).unwrap();
    let mut editor = fixture.editor();
    editor.add_provider("policy", provider(false)).unwrap();
    assert!(editor.save().unwrap());
    let metadata = fs::metadata(&fixture.path).unwrap();
    assert_eq!(metadata.gid(), group);
    assert_eq!(metadata.mode() & 0o777, 0o640);
    let bytes = fixture.bytes();
    chown(&fixture.path, None, Some(original_group)).unwrap();
    assert!(editor.save().is_err());
    assert_eq!(fixture.bytes(), bytes);
    assert_eq!(fs::metadata(&fixture.path).unwrap().gid(), original_group);
    fixture.only_configuration_remains();
}

fn set_read_acl(file: &fs::File, name: &std::ffi::CStr, group_permissions: u16) {
    let mode = file.metadata().unwrap().mode();
    // Linux POSIX ACL xattrs use version 2 and little-endian tag/permission/ID entries.
    let mut acl = 2u32.to_le_bytes().to_vec();
    for (tag, permissions, id) in [
        (1u16, ((mode >> 6) & 7) as u16, u32::MAX),
        (2, 4, 65534),
        (4, group_permissions, u32::MAX),
        (16, 4, u32::MAX),
        (32, (mode & 7) as u16, u32::MAX),
    ] {
        acl.extend(tag.to_le_bytes());
        acl.extend(permissions.to_le_bytes());
        acl.extend(id.to_le_bytes());
    }
    // SAFETY: file owns its descriptor; acl supplies exactly its declared byte length.
    assert_eq!(
        unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                name.as_ptr(),
                acl.as_ptr().cast(),
                acl.len(),
                0,
            )
        },
        0,
        "{}",
        std::io::Error::last_os_error()
    );
}

#[test]
fn inherited_acl_cannot_widen_group_readable_publication() {
    let fixture = Fixture::new(MINIMAL.as_bytes());
    fs::set_permissions(&fixture.path, fs::Permissions::from_mode(0o640)).unwrap();
    let original_group = fs::metadata(&fixture.path).unwrap().gid();
    let parent = fs::File::open(fixture.directory.path()).unwrap();
    set_read_acl(&parent, c"system.posix_acl_default", 4);
    let mut editor = fixture.editor();
    editor.add_provider("policy", provider(false)).unwrap();
    assert!(editor.save().unwrap());
    let published = fs::File::open(&fixture.path).unwrap();
    // SAFETY: published owns its descriptor; a null buffer with zero size queries length.
    assert_eq!(
        unsafe {
            libc::fgetxattr(
                published.as_raw_fd(),
                c"system.posix_acl_access".as_ptr(),
                std::ptr::null_mut(),
                0,
            )
        },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ENODATA)
    );
    assert_eq!(published.metadata().unwrap().mode() & 0o777, 0o640);
    assert_eq!(published.metadata().unwrap().gid(), original_group);
    fixture.only_configuration_remains();
}

#[test]
fn access_acl_masks_are_rejected_on_open_and_after_opening() {
    for after_open in [false, true] {
        let fixture = Fixture::new(MINIMAL.as_bytes());
        fs::set_permissions(&fixture.path, fs::Permissions::from_mode(0o640)).unwrap();
        let inode = fs::metadata(&fixture.path).unwrap().ino();
        let editor = after_open.then(|| fixture.editor());
        let file = fs::File::open(&fixture.path).unwrap();
        set_read_acl(&file, c"system.posix_acl_access", 0);
        assert_eq!(file.metadata().unwrap().mode() & 0o777, 0o640);
        let error = if let Some(mut editor) = editor {
            editor.add_provider("policy", provider(false)).unwrap();
            editor.save().unwrap_err()
        } else {
            ConfigurationEditor::open(&fixture.path).err().unwrap()
        };
        assert!(error.to_string().contains("access ACL"));
        assert_eq!(fixture.bytes(), MINIMAL.as_bytes());
        assert_eq!(fs::metadata(&fixture.path).unwrap().ino(), inode);
        // SAFETY: file owns its descriptor; this only queries the attribute length.
        assert!(unsafe {
            libc::fgetxattr(
                file.as_raw_fd(),
                c"system.posix_acl_access".as_ptr(),
                std::ptr::null_mut(),
                0,
            ) > 0
        });
        fixture.only_configuration_remains();
    }
}

#[test]
fn noops_and_reverted_edits_preserve_exact_bytes_and_inode() {
    let fixture = Fixture::new(MINIMAL.as_bytes());
    let inode = fs::metadata(&fixture.path).unwrap().ino();
    let mut editor = fixture.editor();
    assert!(!editor.remove_provider("absent").unwrap());
    assert!(!editor.remove_hook("absent", "absent").unwrap());
    assert!(!editor.remove_hook("tool.before", "absent").unwrap());
    let event = editor.as_value()["spec"]["events"]["tool.before"].clone();
    assert!(!editor.add_event("tool.before", event).unwrap());
    editor.add_provider("policy", provider(false)).unwrap();
    assert!(!editor.add_provider("policy", provider(false)).unwrap());
    editor
        .add_hook("tool.before", hook("first", "policy", false))
        .unwrap();
    assert!(!editor
        .add_hook("tool.before", hook("first", "policy", false))
        .unwrap());
    editor.remove_hook("tool.before", "first").unwrap();
    editor.remove_provider("policy").unwrap();
    assert!(!editor.save().unwrap());
    assert_eq!(fixture.bytes(), MINIMAL.as_bytes());
    assert_eq!(fs::metadata(&fixture.path).unwrap().ino(), inode);
    fixture.only_configuration_remains();
}

#[test]
fn empty_configuration_requires_explicit_event_and_preserves_disabled_options() {
    let fixture = Fixture::new(MINIMAL.as_bytes());
    let mut value = fixture.editor().as_value().clone();
    value["spec"]["events"] = json!({});
    fs::write(&fixture.path, serde_yaml_ng::to_string(&value).unwrap()).unwrap();
    let mut editor = fixture.editor();
    editor.add_provider("policy", provider(false)).unwrap();
    assert!(editor
        .add_hook("tool.before", hook("first", "policy", false))
        .is_err());
    editor
        .add_event(
            "tool.before",
            json!({"enabled":false,"budget_ms":200,"match":{"tools":["bash"]},"steps":[]}),
        )
        .unwrap();
    editor
        .add_hook("tool.before", hook("first", "policy", false))
        .unwrap();
    assert_eq!(
        editor.as_value()["spec"]["events"]["tool.before"]["enabled"],
        false
    );
    editor.save().unwrap();
    assert_eq!(
        fixture.editor().as_value()["spec"]["events"]["tool.before"]["match"],
        json!({"tools":["bash"]})
    );
}

#[test]
fn conflicts_invalid_steps_and_referenced_removal_leave_working_and_disk_state_intact() {
    let fixture = Fixture::new(MINIMAL.as_bytes());
    let mut editor = fixture.editor();
    editor.add_provider("policy", provider(false)).unwrap();
    let mut disabled = hook("first", "policy", false);
    disabled["enabled"] = json!(false);
    editor.add_hook("tool.before", disabled).unwrap();
    let before = editor.as_value().clone();
    let mut conflicting_provider = provider(false);
    conflicting_provider["timeout_ms"] = json!(99);
    let mut conflicting_hook = hook("first", "policy", false);
    conflicting_hook["operation"] = json!("other");
    let mut invalid_hook = hook("invalid", "policy", false);
    invalid_hook["effects"] = json!(["replace_result"]);
    for result in [
        editor.add_provider("policy", conflicting_provider),
        editor.add_provider("bad/name", provider(false)),
        editor.add_hook("tool.before", conflicting_hook),
        editor.add_hook("tool.before", invalid_hook),
        editor.add_hook("tool.after", hook("unknown", "absent", false)),
        editor.add_hook("tool.before", hook("wrong-shape", "policy", true)),
        editor.add_hook("tool.before", json!({})),
        editor.add_event("unknown.event", json!({"enabled":true,"steps":[]})),
        editor.add_event("tool.before", json!({"enabled":false,"steps":[]})),
        editor.remove_provider("policy"),
    ] {
        assert!(result.is_err());
        assert!(!result.unwrap_err().to_string().contains("private-value"));
    }
    assert_eq!(editor.as_value(), &before);
    assert_eq!(fixture.bytes(), MINIMAL.as_bytes());
    editor.remove_hook("tool.before", "first").unwrap();
    editor.remove_provider("policy").unwrap();
    assert!(!editor.save().unwrap());
    fixture.only_configuration_remains();
}

#[test]
fn stale_snapshots_reject_rename_and_in_place_writes_even_for_noop_saves() {
    for in_place in [false, true] {
        let fixture = Fixture::new(MINIMAL.as_bytes());
        let mut first = fixture.editor();
        let mut stale = fixture.editor();
        first.add_provider("winner", provider(false)).unwrap();
        stale.add_provider("loser", provider(false)).unwrap();
        if in_place {
            fs::write(
                &fixture.path,
                serde_yaml_ng::to_string(first.as_value()).unwrap(),
            )
            .unwrap();
        } else {
            first.save().unwrap();
        }
        let current = fixture.bytes();
        assert!(stale.save().is_err());
        assert_eq!(fixture.bytes(), current);
        let mut noop = ConfigurationEditor::open(&fixture.path).unwrap();
        fs::write(&fixture.path, MINIMAL).unwrap();
        assert!(noop.save().is_err());
        assert_eq!(fixture.bytes(), MINIMAL.as_bytes());
        fixture.only_configuration_remains();
    }
}

#[test]
fn competing_lock_fails_without_replacing_file_and_retry_is_possible() {
    let fixture = Fixture::new(MINIMAL.as_bytes());
    let mut editor = fixture.editor();
    editor.add_provider("policy", provider(false)).unwrap();
    let held = fs::File::open(&fixture.path).unwrap();
    // SAFETY: held owns the live descriptor and remains open during the test.
    assert_eq!(
        unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    assert!(editor.save().is_err());
    assert_eq!(fixture.bytes(), MINIMAL.as_bytes());
    fixture.only_configuration_remains();
    drop(held);
    assert!(editor.save().unwrap());
}

#[test]
fn invalid_input_is_rejected_without_normalizing_or_truncating_the_file() {
    let fixture = Fixture::new(MINIMAL.as_bytes());
    for bytes in [
        MINIMAL
            .replace(
                "kind: AWConfiguration",
                "kind: AWConfiguration\nkind: AWConfiguration",
            )
            .into_bytes(),
        MINIMAL
            .replace("metadata:\n", "unknown: private-value\nmetadata:\n")
            .into_bytes(),
        MINIMAL
            .replace("providers: {}", "providers: []")
            .into_bytes(),
        vec![b' '; aw_config::MAX_DOCUMENT_BYTES + 1],
    ] {
        fs::write(&fixture.path, &bytes).unwrap();
        assert!(ConfigurationEditor::open(&fixture.path).is_err());
        assert_eq!(fixture.bytes(), bytes);
        fixture.only_configuration_remains();
    }
}

#[test]
fn valid_large_json_noops_do_not_depend_on_yaml_formatting_overhead() {
    let fixture = Fixture::new(MINIMAL.as_bytes());
    let mut value = fixture.editor().as_value().clone();
    value["spec"]["providers"]["policy"] = provider(false);
    value["spec"]["providers"]["policy"]["config"]["data"] = json!(vec![0; 500_000]);
    let bytes = serde_json::to_vec(&value).unwrap();
    assert!(bytes.len() < aw_config::MAX_DOCUMENT_BYTES);
    assert!(serde_yaml_ng::to_string(&value).unwrap().len() > aw_config::MAX_DOCUMENT_BYTES);
    fs::write(&fixture.path, &bytes).unwrap();
    let inode = fs::metadata(&fixture.path).unwrap().ino();
    let mut editor = fixture.editor();
    editor.validate().unwrap();
    assert!(!editor.save().unwrap());
    assert_eq!(fixture.bytes(), bytes);
    assert_eq!(fs::metadata(&fixture.path).unwrap().ino(), inode);
    // A real edit cannot publish an oversized YAML file, and must remain staged out.
    assert!(editor.add_provider("other", provider(true)).is_err());
    assert_eq!(editor.as_value(), &value);
    assert_eq!(fixture.bytes(), bytes);
    fixture.only_configuration_remains();
}

#[test]
fn parent_rename_and_symlink_cannot_redirect_an_open_editor() {
    let fixture = Fixture::new(MINIMAL.as_bytes());
    let selected = fixture.directory.path().join("selected");
    let other = fixture.directory.path().join("other");
    let moved = fixture.directory.path().join("moved");
    for directory in [&selected, &other] {
        fs::create_dir(directory).unwrap();
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(directory.join("aw.yaml"), MINIMAL).unwrap();
        fs::set_permissions(directory.join("aw.yaml"), fs::Permissions::from_mode(0o600)).unwrap();
    }
    fs::set_permissions(fixture.directory.path(), fs::Permissions::from_mode(0o777)).unwrap();
    let path = selected.join("aw.yaml");
    let mut editor = ConfigurationEditor::open(&path).unwrap();
    editor.add_provider("policy", provider(false)).unwrap();
    fs::rename(&selected, &moved).unwrap();
    symlink(&other, &selected).unwrap();
    assert!(editor.save().unwrap());
    assert!(!editor.save().unwrap());
    assert_eq!(fs::read(other.join("aw.yaml")).unwrap(), MINIMAL.as_bytes());
    let edited = ConfigurationEditor::open(&moved.join("aw.yaml")).unwrap();
    assert!(edited.provider("policy").is_some());
    assert!(ConfigurationEditor::open(&path).is_err());
    fs::set_permissions(&moved, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(editor.save().is_err());
    fs::set_permissions(&moved, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(fs::read_dir(&moved).unwrap().count(), 1);
    assert_eq!(fs::read_dir(&other).unwrap().count(), 1);
}

#[test]
fn symlinks_hard_links_and_writable_peers_are_rejected() {
    let fixture = Fixture::new(MINIMAL.as_bytes());
    let original = fixture.bytes();
    let alias = fixture.directory.path().join("alias.yaml");
    symlink(&fixture.path, &alias).unwrap();
    assert!(ConfigurationEditor::open(&alias).is_err());
    fs::remove_file(&alias).unwrap();
    fs::hard_link(&fixture.path, &alias).unwrap();
    assert!(ConfigurationEditor::open(&fixture.path).is_err());
    fs::remove_file(&alias).unwrap();
    fs::set_permissions(&fixture.path, fs::Permissions::from_mode(0o666)).unwrap();
    assert!(ConfigurationEditor::open(&fixture.path).is_err());
    fs::set_permissions(&fixture.path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(fixture.directory.path(), fs::Permissions::from_mode(0o777)).unwrap();
    assert!(ConfigurationEditor::open(&fixture.path).is_err());
    fs::set_permissions(fixture.directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(fixture.bytes(), original);
    fixture.only_configuration_remains();
}
