use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

use asc_lua_lsm::{LuaLsmClient, LuaLsmPlan, LuaLsmTranslator, SecurityFs};
use asc_policy_target_contracts::TargetDeploymentClient;
use asc_policy_types::{
    binding::PreparedBinding,
    target::{FailureKind, Presence, TranslationOutcome},
};
use serde_json::json;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReadbackFault {
    RegistrationReadUnavailable,
    ListedAfterUnregister,
    ReplacementMissingAfterUnregister,
}

#[derive(Default)]
struct Kernel {
    names: Mutex<BTreeMap<String, usize>>,
    calls: Mutex<Vec<String>>,
    write_error: Mutex<Option<i32>>,
    short_write: Mutex<bool>,
    fail_bootstrap: Mutex<bool>,
    fail_transfer: Mutex<bool>,
    readback_fault: Mutex<Option<ReadbackFault>>,
    members: Mutex<BTreeMap<String, BTreeSet<u32>>>,
}
impl SecurityFs for Kernel {
    fn read(&self, node: &str) -> io::Result<Vec<u8>> {
        let after_register = {
            let mut calls = self.calls.lock().unwrap();
            let after_register = calls.last().is_some_and(|call| call == "write:register");
            calls.push(format!("read:{node}"));
            after_register
        };
        if node == "version" {
            return Ok(b"1".to_vec());
        }
        if after_register
            && *self.readback_fault.lock().unwrap()
                == Some(ReadbackFault::RegistrationReadUnavailable)
        {
            return Err(io::Error::other("registration readback failed"));
        }
        let mut text = "modules for lua-lsm\nname license size nlsm nload shdict kvnode author\n------------------------------------------\n".to_owned();
        for (name, hooks) in self.names.lock().unwrap().iter() {
            writeln!(&mut text, "{name} GPL-2.0 100 {hooks} 1 0 0 AgentSecCore").unwrap();
        }
        Ok(text.into_bytes())
    }
    fn write(&self, node: &str, payload: &[u8]) -> io::Result<usize> {
        let text = std::str::from_utf8(payload).unwrap();
        self.calls.lock().unwrap().push(format!("write:{node}"));
        if let Some(errno) = self.write_error.lock().unwrap().take() {
            return Err(io::Error::from_raw_os_error(errno));
        }
        if *self.short_write.lock().unwrap() {
            return Ok(payload.len() / 2);
        }
        if node == "register" {
            assert!(text.starts_with("local name = \"asc_"));
            assert!(!text.starts_with('{'));
            let name = text
                .lines()
                .next()
                .unwrap()
                .strip_prefix("local name = \"")
                .unwrap()
                .strip_suffix('"')
                .unwrap();
            let mut names = self.names.lock().unwrap();
            if names.contains_key(name) {
                return Err(io::Error::from_raw_os_error(17));
            }
            let hooks = ["inode_create", "inode_mkdir"]
                .iter()
                .filter(|hook| text.contains(&format!("\n  {hook} = function(")))
                .count();
            names.insert(name.into(), 15 + hooks);
        } else {
            assert_eq!(node, "unregister");
            assert!(!payload.ends_with(b"\n"));
            let fault = *self.readback_fault.lock().unwrap();
            if fault == Some(ReadbackFault::ListedAfterUnregister) {
                return Ok(payload.len());
            }
            let mut names = self.names.lock().unwrap();
            if names.remove(text).is_none() {
                return Err(io::Error::from_raw_os_error(2));
            }
            if fault == Some(ReadbackFault::ReplacementMissingAfterUnregister) {
                names.clear();
            }
        }
        Ok(payload.len())
    }
    fn bootstrap(&self, path: &Path) -> io::Result<()> {
        if *self.fail_bootstrap.lock().unwrap() {
            return Err(io::Error::other("no acknowledgement"));
        }
        let parts: Vec<_> = path
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let index = parts.iter().position(|p| p.starts_with("asc_")).unwrap();
        let name = parts[index].clone();
        let command = &parts[index + 1];
        self.calls
            .lock()
            .unwrap()
            .push(format!("bootstrap:{command}"));
        let pid = parts.last().unwrap().parse::<u32>().ok();
        match command.as_str() {
            "seed" | "seed_root" => {
                self.members
                    .lock()
                    .unwrap()
                    .entry(name)
                    .or_default()
                    .insert(pid.unwrap());
            }
            "member"
                if !self
                    .members
                    .lock()
                    .unwrap()
                    .get(&name)
                    .is_some_and(|set| set.contains(&pid.unwrap())) =>
            {
                return Err(io::Error::from_raw_os_error(3));
            }
            "complete" if *self.fail_transfer.lock().unwrap() => {
                return Err(io::Error::from_raw_os_error(11));
            }
            _ => {}
        }
        Ok(())
    }
}

fn binding(path: &Path) -> PreparedBinding {
    binding_for_pid(path, std::process::id())
}

fn binding_for_pid(path: &Path, pid: u32) -> PreparedBinding {
    let mut binding: PreparedBinding = serde_json::from_str(include_str!(
        "../../asc-policy-types/tests/fixtures/prepared-binding.json"
    ))
    .unwrap();
    let condition = format!(
        "resource.path == {}",
        serde_json::to_string(path.to_str().unwrap()).unwrap()
    );
    let text = format!(
        "forbid(principal, action == SecCore::Action::\"file.read\", resource is SecCore::File) when {{ {condition} }};"
    );
    binding.policy.template = serde_json::from_value(json!({"language":"cedar", "content":{"schema":{"schemaId":"seccore.system","revision":1},"text":text}})).unwrap();
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    binding.scope.process.pid = pid;
    binding.scope.process.start_time = stat[stat.rfind(')').unwrap() + 1..]
        .split_whitespace()
        .nth(19)
        .unwrap()
        .parse()
        .unwrap();
    binding.scope.process.boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .unwrap()
        .trim()
        .into();
    binding.scope.process.pid_namespace = fs::read_link(format!("/proc/{pid}/ns/pid"))
        .unwrap()
        .to_string_lossy()
        .into_owned();
    binding.scope.selector = asc_policy_types::scope::ScopeSelector::Pid { pid };
    binding
}

fn permission_binding(path: &Path, pid: u32, operations: &[&str], tree: bool) -> PreparedBinding {
    let mut binding = binding_for_pid(path, pid);
    binding.binding_id =
        asc_policy_types::identifiers::ResourceId::new(uuid::Uuid::new_v4().to_string()).unwrap();
    let quote = |path: &Path| serde_json::to_string(path.to_str().unwrap()).unwrap();
    let selector = if tree {
        format!(
            "(resource.path == {} || resource.path like {})",
            quote(path),
            quote(&path.join("*"))
        )
    } else {
        format!("resource.path == {}", quote(path))
    };
    let mut text = String::new();
    for operation in operations {
        if *operation == "read" {
            write!(
                &mut text,
                r#"forbid(principal, action == SecCore::Action::"file.read", resource) when {{ {selector} }};"#
            ).unwrap();
        } else {
            assert!(matches!(*operation, "write" | "delete"));
            write!(
                &mut text,
                r#"forbid(principal, action == SecCore::Action::"file.write", resource)
                when {{ context.input.operation == SecCore::WriteOperation::"{operation}" && {selector} }};"#
            ).unwrap();
        }
    }
    binding.policy.template = serde_json::from_value(json!({"language":"cedar", "content":{"schema":{"schemaId":"seccore.system","revision":1},"text":text}})).unwrap();
    binding
}

fn prepared(
    client: &LuaLsmClient,
    state: &Path,
    binding: &PreparedBinding,
) -> asc_policy_types::target::PreparedApply {
    let outcome = LuaLsmTranslator::new(state).translate(binding).unwrap();
    let TranslationOutcome::Translated(plan) = outcome else {
        panic!("translation rejected: {outcome:?}");
    };
    client.prepare_apply(&plan).unwrap()
}

#[test]
fn pure_preparation_and_native_create_update_delete_keep_the_contract() {
    let subject = Probe::start();
    let directory = tempfile::tempdir().unwrap();
    let protected = directory.path().join("config");
    fs::write(&protected, b"secret").unwrap();
    let state = directory.path().join("state");
    let kernel = Arc::new(Kernel::default());
    let client = LuaLsmClient::new(kernel.clone(), &state);
    let translator = LuaLsmTranslator::new(&state);
    let mut binding = binding_for_pid(&protected, subject.child.id());
    let TranslationOutcome::Translated(plan) = translator.translate(&binding).unwrap() else {
        panic!("translate");
    };
    let prepared = client.prepare_apply(&plan).unwrap();
    assert!(!state.exists());
    assert!(kernel.calls.lock().unwrap().is_empty());
    let report = client.create(&prepared);
    assert_eq!(report.error, None);
    assert_eq!(report.observations[0].presence, Presence::Present);
    let calls = kernel.calls.lock().unwrap().len();
    assert_eq!(client.update(&[], &prepared).error, None);
    assert_eq!(
        &kernel.calls.lock().unwrap()[calls..],
        ["read:version", "read:modules"]
    );
    // A fresh Client adopts only confirmed same-boot, same-source registrations.
    let client = LuaLsmClient::new(kernel.clone(), &state);
    let calls = kernel.calls.lock().unwrap().len();
    assert_eq!(client.create(&prepared).error, None);
    assert_eq!(
        &kernel.calls.lock().unwrap()[calls..],
        ["read:version", "read:modules"]
    );
    // Identical source does not prove attachment to another process instance.
    let mut stale_subject: LuaLsmPlan = serde_json::from_slice(&plan.content).unwrap();
    stale_subject.process.start_time += 1;
    let mut forged_plan = plan.clone();
    forged_plan.content = serde_json::to_vec(&stale_subject).unwrap();
    let incompatible = client.prepare_apply(&forged_plan).unwrap();
    assert_eq!(
        client.create(&incompatible).error.unwrap().code,
        "LUA_LSM_EXISTING_MODULE_UNCONFIRMED"
    );
    binding.binding_revision = binding.binding_revision.checked_next().unwrap();
    let TranslationOutcome::Translated(plan) = translator.translate(&binding).unwrap() else {
        panic!("translate");
    };
    let next = client.prepare_apply(&plan).unwrap();
    let start = kernel.calls.lock().unwrap().len();
    let result = client.update(std::slice::from_ref(&prepared.target), &next);
    assert_eq!(result.error, None);
    assert_eq!(
        result
            .observations
            .iter()
            .map(|o| o.presence)
            .collect::<Vec<_>>(),
        [Presence::Present, Presence::Absent]
    );
    let calls = kernel.calls.lock().unwrap();
    let changes: Vec<_> = calls[start..]
        .iter()
        .filter(|c| c.starts_with("write:"))
        .cloned()
        .collect();
    assert_eq!(changes, ["write:register", "write:unregister"]);
    assert!(calls.ends_with(&[
        "write:unregister".into(),
        "read:modules".into(),
        "read:modules".into()
    ]));
    drop(calls);
    assert_eq!(
        client.delete(std::slice::from_ref(&next.target)).error,
        None
    );
    assert_eq!(client.delete(&[next.target]).error, None);
    assert!(kernel.names.lock().unwrap().is_empty());
}

#[test]
fn acknowledged_writes_require_registration_and_absence_readback() {
    let subject = Probe::start();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("file");
    fs::write(&path, b"data").unwrap();
    let state = directory.path().join("state");
    let kernel = Arc::new(Kernel::default());
    let client = LuaLsmClient::new(kernel.clone(), &state);
    let mut binding = binding_for_pid(&path, subject.child.id());
    let old = prepared(&client, &state, &binding);
    assert_eq!(client.create(&old).error, None);
    let previous = std::slice::from_ref(&old.target);
    binding.binding_revision = binding.binding_revision.checked_next().unwrap();
    let new = prepared(&client, &state, &binding);

    *kernel.readback_fault.lock().unwrap() = Some(ReadbackFault::RegistrationReadUnavailable);
    let pending = client.update(previous, &new);
    assert_eq!(pending.error.unwrap().code, "LUA_LSM_QUERY_UNCONFIRMED");
    assert_eq!(pending.observations[0].presence, Presence::Unknown);
    assert!(kernel.names.lock().unwrap().contains_key(&old.target.id));
    assert!(kernel.names.lock().unwrap().contains_key(&new.target.id));

    // A new Client can finish initialization after losing registration readback.
    let client = LuaLsmClient::new(kernel.clone(), &state);
    *kernel.readback_fault.lock().unwrap() = Some(ReadbackFault::ListedAfterUnregister);
    let pending = client.update(previous, &new);
    assert_eq!(
        pending.error.unwrap().code,
        "LUA_LSM_UNREGISTER_ABSENCE_UNCONFIRMED"
    );
    assert_eq!(pending.observations[0].presence, Presence::Present);
    assert_eq!(pending.observations[1].presence, Presence::Unknown);
    assert!(state.join(&old.target.id).is_dir());
    assert_eq!(
        kernel
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| *call == "write:register")
            .count(),
        2
    );

    *kernel.readback_fault.lock().unwrap() = Some(ReadbackFault::ReplacementMissingAfterUnregister);
    let pending = client.update(previous, &new);
    assert_eq!(
        pending.error.unwrap().code,
        "LUA_LSM_REGISTRATION_UNCONFIRMED"
    );
    assert_eq!(pending.observations[0].presence, Presence::Unknown);
    assert_eq!(pending.observations[1].presence, Presence::Absent);

    *kernel.readback_fault.lock().unwrap() = None;
    let confirmed = client.update(previous, &new);
    assert_eq!(confirmed.error, None);
    assert_eq!(confirmed.observations[0].presence, Presence::Present);
    assert_eq!(confirmed.observations[1].presence, Presence::Absent);
    assert!(!state.join(&old.target.id).exists());
}

#[test]
fn proc_fd_roundtrip_keeps_the_pinned_symlink_inode() {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("target"), b"data").unwrap();
    let link = directory.path().join("link");
    std::os::unix::fs::symlink("target", &link).unwrap();
    let flags = rustix::fs::OFlags::PATH | rustix::fs::OFlags::NOFOLLOW;
    let pinned = fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags.bits().cast_signed())
        .open(&link)
        .unwrap();
    let reopened = fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::PATH.bits().cast_signed())
        .open(format!("/proc/self/fd/{}", pinned.as_raw_fd()))
        .unwrap();
    let a = pinned.metadata().unwrap();
    let b = reopened.metadata().unwrap();
    assert!(b.file_type().is_symlink());
    assert_eq!((a.dev(), a.ino()), (b.dev(), b.ino()));
}

#[test]
fn writable_state_ancestors_are_rejected_before_kernel_io() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let unsafe_parent = directory.path().join("unsafe");
    fs::create_dir(&unsafe_parent).unwrap();
    fs::set_permissions(&unsafe_parent, fs::Permissions::from_mode(0o777)).unwrap();
    let state = unsafe_parent.join("state");
    let kernel = Arc::new(Kernel::default());
    let client = LuaLsmClient::new(kernel.clone(), &state);
    let TranslationOutcome::Translated(plan) = LuaLsmTranslator::new(&state)
        .translate(&binding(&directory.path().join("file")))
        .unwrap()
    else {
        panic!("translate");
    };
    let prepared = client.prepare_apply(&plan).unwrap();
    assert_eq!(
        client.create(&prepared).error.unwrap().kind,
        FailureKind::Rejected
    );
    assert!(!state.exists());
    assert!(kernel.calls.lock().unwrap().is_empty());
}

#[test]
fn short_writes_conflicts_and_incomplete_initialization_stay_unknown() {
    let subject = Probe::start();
    let directory = tempfile::tempdir().unwrap();
    let protected = directory.path().join("file");
    fs::write(&protected, b"x").unwrap();
    let state = directory.path().join("state");
    let kernel = Arc::new(Kernel::default());
    let client = LuaLsmClient::new(kernel.clone(), &state);
    let TranslationOutcome::Translated(plan) = LuaLsmTranslator::new(&state)
        .translate(&binding_for_pid(&protected, subject.child.id()))
        .unwrap()
    else {
        panic!("translate");
    };
    let prepared = client.prepare_apply(&plan).unwrap();
    *kernel.short_write.lock().unwrap() = true;
    let result = client.create(&prepared);
    assert_eq!(result.error.unwrap().code, "LUA_LSM_REGISTER_SHORT_WRITE");
    assert_eq!(result.observations[0].presence, Presence::Unknown);
    assert_eq!(
        kernel
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| *c == "write:register")
            .count(),
        1
    );
    *kernel.short_write.lock().unwrap() = false;
    *kernel.write_error.lock().unwrap() = Some(17);
    let result = client.create(&prepared);
    assert_eq!(result.error.unwrap().code, "LUA_LSM_REGISTER_NAME_CONFLICT");
    assert_eq!(result.observations[0].presence, Presence::Unknown);
    assert!(!state.join(format!("{}.json", prepared.target.id)).exists());
    *kernel.fail_bootstrap.lock().unwrap() = true;
    let result = client.create(&prepared);
    assert_eq!(
        result.error.unwrap().code,
        "LUA_LSM_INITIALIZATION_UNCONFIRMED"
    );
    assert_eq!(result.observations[0].presence, Presence::Unknown);
    *kernel.fail_bootstrap.lock().unwrap() = false;
    let client = LuaLsmClient::new(kernel.clone(), &state);
    let resume = kernel.calls.lock().unwrap().len();
    assert_eq!(client.create(&prepared).error, None);
    {
        let calls = kernel.calls.lock().unwrap();
        let resumed = &calls[resume..];
        assert!(resumed.starts_with(&["read:version".into(), "read:modules".into()]));
        assert!(resumed.iter().any(|call| call == "bootstrap:seed_root"));
        assert!(resumed.iter().all(|call| call != "write:register"));
    }
    assert_eq!(
        kernel
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| *c == "write:register")
            .count(),
        3
    );
    // A completed receipt alone cannot restore a module lost from the kernel.
    kernel.names.lock().unwrap().remove(&prepared.target.id);
    kernel.members.lock().unwrap().remove(&prepared.target.id);
    let restarted = LuaLsmClient::new(kernel.clone(), &state);
    let start = kernel.calls.lock().unwrap().len();
    assert_eq!(restarted.create(&prepared).error, None);
    {
        let calls = kernel.calls.lock().unwrap();
        let recovered = &calls[start..];
        assert!(recovered.starts_with(&[
            "read:version".into(),
            "read:modules".into(),
            "write:register".into(),
            "read:modules".into(),
            "bootstrap:root_candidate".into(),
            "bootstrap:seed_root".into(),
        ]));
    }
    assert_eq!(client.delete(&[prepared.target]).error, None);
}

#[test]
fn failed_update_preserves_old_modules_and_busy_delete_invalidates_evidence() {
    let subject = Probe::start();
    let directory = tempfile::tempdir().unwrap();
    let protected = directory.path().join("file");
    fs::write(&protected, b"x").unwrap();
    let state = directory.path().join("state");
    let kernel = Arc::new(Kernel::default());
    let client = LuaLsmClient::new(kernel.clone(), &state);
    let translator = LuaLsmTranslator::new(&state);
    let mut binding = binding_for_pid(&protected, subject.child.id());
    let TranslationOutcome::Translated(plan) = translator.translate(&binding).unwrap() else {
        panic!("translate");
    };
    let old = client.prepare_apply(&plan).unwrap();
    assert_eq!(client.create(&old).error, None);
    binding.binding_revision = binding.binding_revision.checked_next().unwrap();
    let TranslationOutcome::Translated(plan) = translator.translate(&binding).unwrap() else {
        panic!("translate");
    };
    let next = client.prepare_apply(&plan).unwrap();
    *kernel.write_error.lock().unwrap() = Some(1);
    assert_eq!(
        client
            .update(std::slice::from_ref(&old.target), &next)
            .error
            .unwrap()
            .kind,
        FailureKind::Rejected
    );
    assert!(kernel.names.lock().unwrap().contains_key(&old.target.id));
    *kernel.write_error.lock().unwrap() = Some(16);
    let failed = client.delete(std::slice::from_ref(&old.target));
    assert_eq!(failed.observations[0].presence, Presence::Unknown);
    assert_eq!(
        failed.error.unwrap().code,
        "LUA_LSM_UNREGISTER_BUSY_UNCONFIRMED"
    );
    assert_eq!(
        client.create(&old).error.unwrap().code,
        "LUA_LSM_EXISTING_MODULE_UNCONFIRMED"
    );
    *kernel.write_error.lock().unwrap() = Some(2);
    let missing_control = client.delete(std::slice::from_ref(&old.target));
    assert_eq!(missing_control.observations[0].presence, Presence::Unknown);
    assert_eq!(
        missing_control.error.unwrap().code,
        "LUA_LSM_UNREGISTER_ABSENCE_UNCONFIRMED"
    );
    let mut invalid = old.target.clone();
    invalid.id = "unrelated_module".into();
    let before = kernel.calls.lock().unwrap().len();
    assert!(client.delete(&[old.target, invalid]).error.is_some());
    assert_eq!(kernel.calls.lock().unwrap().len(), before);
}

#[test]
fn preparation_rejects_edited_source_and_translation_is_deterministic() {
    let directory = tempfile::tempdir().unwrap();
    let binding = binding(&directory.path().join("不存在\""));
    let translator = LuaLsmTranslator::new(directory.path().join("state"));
    let TranslationOutcome::Translated(mut plan) = translator.translate(&binding).unwrap() else {
        panic!("translate");
    };
    assert_eq!(
        translator.translate(&binding).unwrap(),
        TranslationOutcome::Translated(plan.clone())
    );
    let mut decoded: LuaLsmPlan = serde_json::from_slice(&plan.content).unwrap();
    decoded.source.push_str("\nerror('injected')");
    plan.content = serde_json::to_vec(&decoded).unwrap();
    let client = LuaLsmClient::new(Arc::new(Kernel::default()), directory.path().join("state"));
    assert_eq!(
        client.prepare_apply(&plan).unwrap_err().code,
        "LUA_LSM_INVALID_PLAN"
    );
}

#[test]
fn incomplete_handover_keeps_the_old_module_and_retries_the_same_instance() {
    let subject = Probe::start();
    let directory = tempfile::tempdir().unwrap();
    let protected = directory.path().join("file");
    fs::write(&protected, b"secret").unwrap();
    let state = directory.path().join("state");
    let kernel = Arc::new(Kernel::default());
    let client = LuaLsmClient::new(kernel.clone(), &state);
    let translator = LuaLsmTranslator::new(&state);
    let mut binding = permission_binding(&protected, subject.child.id(), &["write"], false);
    let TranslationOutcome::Translated(plan) = translator.translate(&binding).unwrap() else {
        panic!("translate");
    };
    let first = client.prepare_apply(&plan).unwrap();
    assert_eq!(client.create(&first).error, None);
    // A legacy writer can transfer members but cannot confirm the new guarantees.
    kernel
        .names
        .lock()
        .unwrap()
        .insert(first.target.id.clone(), 15);
    assert_eq!(
        client.create(&first).error.unwrap().code,
        "LUA_LSM_UNSUPPORTED_HOOK_SET"
    );
    binding.binding_revision = binding.binding_revision.checked_next().unwrap();
    let TranslationOutcome::Translated(plan) = translator.translate(&binding).unwrap() else {
        panic!("translate");
    };
    let next = client.prepare_apply(&plan).unwrap();
    *kernel.fail_transfer.lock().unwrap() = true;
    let report = client.update(std::slice::from_ref(&first.target), &next);
    assert_eq!(
        report.error.unwrap().code,
        "LUA_LSM_MEMBERSHIP_TRANSFER_PENDING"
    );
    assert!(kernel.names.lock().unwrap().contains_key(&first.target.id));
    assert_eq!(report.observations[0].presence, Presence::Unknown);
    *kernel.fail_transfer.lock().unwrap() = false;
    assert_eq!(client.update(&[first.target], &next).error, None);
    assert_eq!(
        kernel
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| *call == "write:register")
            .count(),
        2
    );
}

#[test]
fn revision_handover_uses_old_membership_after_the_original_root_is_reaped() {
    let directory = tempfile::tempdir().unwrap();
    let protected = directory.path().join("file");
    fs::write(&protected, b"secret").unwrap();
    let state = directory.path().join("state");
    let kernel = Arc::new(Kernel::default());
    let client = LuaLsmClient::new(kernel.clone(), &state);
    let translator = LuaLsmTranslator::new(&state);
    let mut root = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let mut binding = binding_for_pid(&protected, root.id());
    let TranslationOutcome::Translated(plan) = translator.translate(&binding).unwrap() else {
        panic!("translate");
    };
    let first = client.prepare_apply(&plan).unwrap();
    let deployed = client.create(&first);
    root.kill().unwrap();
    root.wait().unwrap();
    assert_eq!(deployed.error, None);
    // Model a surviving orphan which the old template still owns.
    kernel
        .members
        .lock()
        .unwrap()
        .get_mut(&first.target.id)
        .unwrap()
        .insert(std::process::id());
    binding.binding_revision = binding.binding_revision.checked_next().unwrap();
    let TranslationOutcome::Translated(plan) = translator.translate(&binding).unwrap() else {
        panic!("translate");
    };
    let next = client.prepare_apply(&plan).unwrap();
    assert_eq!(client.update(&[first.target], &next).error, None);
    assert!(kernel.members.lock().unwrap()[&next.target.id].contains(&std::process::id()));
}

#[test]
#[ignore = "requires a Lua 5.1 interpreter; set LUA_BIN and run explicitly"]
fn lua51_template_behavior() {
    let executable = std::env::var_os("LUA_BIN").unwrap_or_else(|| "lua5.1".into());
    let status = std::process::Command::new(executable)
        .args([
            "tests/template.lua",
            "../asc-lua-lsm-protocol/src/file_protection.lua",
        ])
        .status()
        .unwrap();
    assert!(status.success());
}

struct Probe {
    child: std::process::Child,
    input: Option<std::process::ChildStdin>,
    output: std::io::BufReader<std::process::ChildStdout>,
}
impl Probe {
    fn start() -> Self {
        Self::with_namespaces(&[])
    }
    fn with_namespaces(options: &[&str]) -> Self {
        let code = include_str!("probe.py");
        let mut command = std::process::Command::new("python3");
        if !options.is_empty() {
            command = std::process::Command::new("unshare");
            command.args(options).arg("python3");
        }
        let mut child = command
            .args(["-u", "-c", code])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let output = std::io::BufReader::new(child.stdout.take().unwrap());
        // Child::wait closes child.stdin; an orphan must keep the command pipe.
        let input = child.stdin.take();
        Self {
            child,
            input,
            output,
        }
    }
    fn run(&mut self, operation: &str, path: &Path) -> String {
        use std::io::{BufRead, Write};
        writeln!(
            self.input.as_mut().unwrap(),
            "{operation} {}",
            path.display()
        )
        .unwrap();
        let mut response = String::new();
        self.output.read_line(&mut response).unwrap();
        response.trim().into()
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        drop(self.input.take());
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

struct Installed<'a> {
    client: &'a LuaLsmClient,
    targets: Vec<asc_policy_types::target::TargetRef>,
}

// Exercise the same retryable handover/unregister results as the reconciler.
fn settle_native(
    mut request: impl FnMut() -> asc_policy_types::target::DeploymentReport,
) -> asc_policy_types::target::DeploymentReport {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let report = request();
        if !report.error.as_ref().is_some_and(|error| {
            matches!(
                error.code.as_str(),
                "LUA_LSM_MEMBERSHIP_TRANSFER_PENDING" | "LUA_LSM_UNREGISTER_BUSY_UNCONFIRMED"
            )
        }) || std::time::Instant::now() >= deadline
        {
            return report;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

impl Drop for Installed<'_> {
    fn drop(&mut self) {
        let report = settle_native(|| self.client.delete(&self.targets));
        if report.error.is_some() {
            eprintln!("native cleanup unconfirmed: {report:?}");
        }
    }
}

#[test]
#[ignore = "requires a disposable host with Lua-LSM v1, root CAP_MAC_ADMIN and python3; private targets only"]
fn live_kernel_exact_file_alias_existing_fd_update_and_orphan() {
    assert!(Path::new("/sys/kernel/security/lua/register").exists());
    let directory = tempfile::tempdir().unwrap();
    let protected = directory.path().join("protected");
    let alias = directory.path().join("alias");
    fs::write(&protected, b"secret").unwrap();
    fs::hard_link(&protected, &alias).unwrap();
    let state = directory.path().join("state");
    let client = LuaLsmClient::new(Arc::new(asc_lua_lsm::NativeSecurityFs::default()), &state);
    let mut installed = Installed {
        client: &client,
        targets: vec![],
    };
    let mut probe = Probe::start();
    assert_eq!(probe.run("hold", &protected), "ok");
    let mut binding = binding_for_pid(&protected, probe.child.id());
    binding.binding_id =
        asc_policy_types::identifiers::ResourceId::new(uuid::Uuid::new_v4().to_string()).unwrap();
    let translator = LuaLsmTranslator::new(&state);
    let TranslationOutcome::Translated(plan) = translator.translate(&binding).unwrap() else {
        panic!("translate");
    };
    let first = client.prepare_apply(&plan).unwrap();
    installed.targets.push(first.target.clone());
    assert_eq!(client.create(&first).error, None);
    assert_eq!(probe.run("read", &protected), "errno=1");
    assert_eq!(probe.run("read", &alias), "errno=1");
    assert_eq!(probe.run("readfd", &protected), "errno=1");
    assert_eq!(fs::read(&protected).unwrap(), b"secret");
    // A same-source retry uses a live callback acknowledgement without replacing.
    assert_eq!(client.update(&[], &first).error, None);
    binding.binding_revision = binding.binding_revision.checked_next().unwrap();
    let text = format!(
        r#"forbid(principal, action == SecCore::Action::"file.write", resource)
        when {{ [SecCore::WriteOperation::"write", SecCore::WriteOperation::"delete"].contains(context.input.operation)
            && resource.path == {} }};"#,
        serde_json::to_string(protected.to_str().unwrap()).unwrap()
    );
    binding.policy.template = serde_json::from_value(json!({"language":"cedar", "content":{"schema":{"schemaId":"seccore.system","revision":1},"text":text}})).unwrap();
    let TranslationOutcome::Translated(plan) = translator.translate(&binding).unwrap() else {
        panic!("translate");
    };
    let next = client.prepare_apply(&plan).unwrap();
    installed.targets.push(next.target.clone());
    assert_eq!(
        settle_native(|| client.update(std::slice::from_ref(&first.target), &next)).error,
        None
    );
    assert_eq!(probe.run("read", &protected), "ok");
    assert_eq!(probe.run("write", &alias), "errno=1");
    assert_eq!(probe.run("delete", &protected), "errno=1");
    assert!(probe.run("orphan", &protected).starts_with("child="));
    probe.child.wait().unwrap();
    assert_eq!(probe.run("write", &alias), "errno=1");
    let restarted = LuaLsmClient::new(Arc::new(asc_lua_lsm::NativeSecurityFs::default()), &state);
    assert_eq!(restarted.update(&[], &next).error, None);
    binding.binding_revision = binding.binding_revision.checked_next().unwrap();
    let TranslationOutcome::Translated(plan) = translator.translate(&binding).unwrap() else {
        panic!("translate");
    };
    let orphan_revision = client.prepare_apply(&plan).unwrap();
    installed.targets.push(orphan_revision.target.clone());
    assert_eq!(
        settle_native(|| client.update(std::slice::from_ref(&next.target), &orphan_revision)).error,
        None
    );
    assert_eq!(probe.run("write", &alias), "errno=1");
    assert_eq!(
        settle_native(|| client.delete(std::slice::from_ref(&orphan_revision.target))).error,
        None
    );
    assert_eq!(probe.run("write", &protected), "ok");
    assert_eq!(
        settle_native(|| client.delete(&installed.targets)).error,
        None
    );
    installed.targets.clear();
}

#[test]
#[ignore = "requires a disposable host with Lua-LSM v1, root CAP_MAC_ADMIN and python3; private targets only"]
fn live_kernel_tree_creation_exclusions_and_aliases() {
    assert!(Path::new("/sys/kernel/security/lua/register").exists());
    let directory = tempfile::tempdir().unwrap();
    let tree = directory.path().join("tree");
    let excluded = tree.join("cache");
    fs::create_dir_all(&excluded).unwrap();
    let protected = tree.join("data");
    let outside = directory.path().join("outside-alias");
    let cached = excluded.join("allowed");
    fs::write(&protected, b"secret").unwrap();
    fs::write(&cached, b"cache").unwrap();
    fs::hard_link(&protected, &outside).unwrap();
    let mut probe = Probe::start();
    let mut binding = binding_for_pid(&protected, probe.child.id());
    binding.binding_id =
        asc_policy_types::identifiers::ResourceId::new(uuid::Uuid::new_v4().to_string()).unwrap();
    let quote = |path: &Path| serde_json::to_string(path.to_str().unwrap()).unwrap();
    let selector = format!(
        "resource.path == {} || resource.path like {}",
        quote(&tree),
        quote(&tree.join("*"))
    );
    let exclusion = format!(
        "resource.path == {} || resource.path like {}",
        quote(&excluded),
        quote(&excluded.join("*"))
    );
    let text = format!(
        r#"forbid(principal, action == SecCore::Action::"file.read", resource)
        when {{ {selector} }} unless {{ {exclusion} }};
        forbid(principal, action == SecCore::Action::"file.write", resource)
        when {{ [SecCore::WriteOperation::"write", SecCore::WriteOperation::"delete"].contains(context.input.operation)
            && ({selector}) }} unless {{ {exclusion} }};"#
    );
    binding.policy.template = serde_json::from_value(json!({"language":"cedar", "content":{"schema":{"schemaId":"seccore.system","revision":1},"text":text}})).unwrap();
    let state = directory.path().join("state");
    let client = LuaLsmClient::new(Arc::new(asc_lua_lsm::NativeSecurityFs::default()), &state);
    let mut installed = Installed {
        client: &client,
        targets: vec![],
    };
    let TranslationOutcome::Translated(plan) =
        LuaLsmTranslator::new(&state).translate(&binding).unwrap()
    else {
        panic!("translate tree");
    };
    let prepared = client.prepare_apply(&plan).unwrap();
    installed.targets.push(prepared.target.clone());
    assert_eq!(client.create(&prepared).error, None);
    for path in [&protected, &outside] {
        for operation in ["read", "write", "delete"] {
            assert_eq!(probe.run(operation, path), "errno=1");
        }
    }
    let created = tree.join("created-after-deploy");
    fs::write(&created, b"new").unwrap();
    assert_eq!(probe.run("read", &created), "errno=1");
    for operation in ["create", "mkdir"] {
        let denied = tree.join(operation);
        assert_eq!(probe.run(operation, &denied), "errno=1");
        assert!(!denied.try_exists().unwrap());
        let allowed = excluded.join(operation);
        assert_eq!(probe.run(operation, &allowed), "ok");
        assert!(allowed.try_exists().unwrap());
    }
    for operation in ["read", "write", "delete"] {
        assert_eq!(probe.run(operation, &cached), "ok");
    }
    assert_eq!(
        settle_native(|| client.delete(&installed.targets)).error,
        None
    );
    installed.targets.clear();
    assert_eq!(probe.run("read", &protected), "ok");
}

fn mapping_test_tempdir() -> tempfile::TempDir {
    // The preflight joins stat and /proc/<pid>/maps by device and inode. On the
    // Linux 6.6 test host, overlayfs reported the overlay device in stat and the
    // backing device in maps. This reproduced the missing preflight error in
    // GitHub Actions job 137576531148 (expected LUA_LSM_PREEXISTING_READ_MAPPING).
    // Linux 6.8 gained the file_user_inode fix (upstream commit 3efdc78fdc21).
    // Probe the actual filesystem to handle both kernel backports and mount
    // differences. Keep the CI assertions active on a verified fallback.
    type Identity = (u32, u32, u64);
    let mut probe = Probe::start();
    let mut identities = |directory: &Path| {
        let file = tempfile::NamedTempFile::new_in(directory).expect("create a mapping probe file");
        file.as_file()
            .set_len(4096)
            .expect("size the mapping probe file");
        let response = probe.run("mapping-identities", file.path());
        serde_json::from_str::<[Identity; 2]>(&response).unwrap_or_else(|error| {
            panic!(
                "Cannot compare stat and /proc/self/maps for {}: {error}. Probe response: {response:?}",
                file.path().display()
            )
        })
    };
    let directory = tempfile::tempdir().unwrap();
    let [stat, maps] = identities(directory.path());
    if stat == maps {
        return directory;
    }

    let kernel = fs::read_to_string("/proc/sys/kernel/osrelease")
        .unwrap_or_else(|error| format!("unknown ({error})"));
    let filesystem = rustix::fs::statfs(directory.path()).map_or_else(
        |error| format!("unknown ({error})"),
        |stat| format!("{:#x}", stat.f_type),
    );
    eprintln!(
        "Mapping fixture at {} on Linux {} (filesystem magic {filesystem}) has different \
         (major, minor, inode) values: stat={stat:?}, /proc/self/maps={maps:?}. \
         Trying tmpfs at /dev/shm.",
        directory.path().display(),
        kernel.trim()
    );

    let fallback = tempfile::tempdir_in("/dev/shm")
        .expect("create a mapping-test directory under /dev/shm after a file identity mismatch");
    let tmpfs_magic = 0x0102_1994;
    assert_eq!(
        rustix::fs::statfs(fallback.path())
            .expect("identify the mapping-test fallback filesystem")
            .f_type,
        tmpfs_magic,
        "mapping-test fallback {} must be tmpfs",
        fallback.path().display()
    );
    let [stat, maps] = identities(fallback.path());
    assert_eq!(
        stat,
        maps,
        "stat and /proc/self/maps must agree in mapping-test fallback {}",
        fallback.path().display()
    );
    eprintln!(
        "Using tmpfs for mapping fixtures at {}",
        fallback.path().display()
    );
    fallback
}

#[test]
fn preexisting_mapping_preflight_rejects_before_registration_and_preserves_old_policy() {
    let mapping_directory = mapping_test_tempdir();
    for tree in [false, true] {
        for (operation, mapping, expected) in [
            (
                "read",
                "holdmap-private-read",
                Some("LUA_LSM_PREEXISTING_READ_MAPPING"),
            ),
            (
                "read",
                "holdmap-private-none",
                Some("LUA_LSM_PREEXISTING_READ_MAPPING"),
            ),
            (
                "read",
                "holdmap-shared-read",
                Some("LUA_LSM_PREEXISTING_READ_MAPPING"),
            ),
            (
                "write",
                "holdmap-shared-write",
                Some("LUA_LSM_PREEXISTING_WRITABLE_MAPPING"),
            ),
            ("write", "holdmap-private-write", None),
            ("write", "holdmap-shared-read", None),
            ("delete", "holdmap-shared-write", None),
        ] {
            let directory = tempfile::tempdir_in(mapping_directory.path()).unwrap();
            let root = directory.path().join("tree");
            fs::create_dir(&root).unwrap();
            let path = root.join("protected");
            fs::write(&path, [b's'; 4096]).unwrap();
            let alias = directory.path().join("alias");
            fs::hard_link(&path, &alias).unwrap();
            let mut probe = Probe::start();
            assert_eq!(probe.run(mapping, &alias), "ok");
            let kernel = Arc::new(Kernel::default());
            let state = directory.path().join("state");
            let client = LuaLsmClient::new(kernel.clone(), &state);
            let binding = permission_binding(
                if tree { &root } else { &path },
                probe.child.id(),
                &[operation],
                tree,
            );
            let plan = prepared(&client, &state, &binding);
            let report = client.create(&plan);
            assert_eq!(
                report.error.as_ref().map(|error| error.code.as_str()),
                expected,
                "{mapping}"
            );
            if expected.is_some() {
                assert_eq!(report.observations[0].presence, Presence::Unknown);
                assert!(kernel.names.lock().unwrap().is_empty());
                assert!(
                    !kernel
                        .calls
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|call| call == "write:register")
                );
            }
        }
    }
    let directory = tempfile::tempdir_in(mapping_directory.path()).unwrap();
    let path = directory.path().join("protected");
    fs::write(&path, [b's'; 4096]).unwrap();
    let mut probe = Probe::start();
    let state = directory.path().join("state");
    let kernel = Arc::new(Kernel::default());
    let client = LuaLsmClient::new(kernel.clone(), &state);
    let mut binding = permission_binding(&path, probe.child.id(), &["write"], false);
    let old = prepared(&client, &state, &binding);
    assert_eq!(client.create(&old).error, None);
    assert_eq!(probe.run("holdmap-private-read", &path), "ok");
    let mut replacement = permission_binding(&path, probe.child.id(), &["read"], false);
    replacement.binding_id = binding.binding_id;
    binding.binding_revision = binding.binding_revision.checked_next().unwrap();
    replacement.binding_revision = binding.binding_revision;
    let next = prepared(&client, &state, &replacement);
    let report = client.update(std::slice::from_ref(&old.target), &next);
    assert_eq!(
        report.error.unwrap().code,
        "LUA_LSM_PREEXISTING_READ_MAPPING"
    );
    assert!(kernel.names.lock().unwrap().contains_key(&old.target.id));
    assert_eq!(report.observations[0].presence, Presence::Unknown);
    assert!(!kernel.members.lock().unwrap().contains_key(&next.target.id));
}

#[test]
fn preexisting_descendant_mapping_and_exclusions_follow_policy_scope() {
    let directory = mapping_test_tempdir();
    let root = directory.path().join("tree");
    fs::create_dir(&root).unwrap();
    let path = root.join("cached");
    fs::write(&path, [b's'; 4096]).unwrap();
    let mut probe = Probe::start();
    assert_eq!(probe.run("spawn-child-map", &path), "ok");
    let state = directory.path().join("state");
    let kernel = Arc::new(Kernel::default());
    let client = LuaLsmClient::new(kernel.clone(), &state);
    let mut binding = permission_binding(&root, probe.child.id(), &["read"], true);
    let plan = prepared(&client, &state, &binding);
    assert_eq!(
        client.create(&plan).error.unwrap().code,
        "LUA_LSM_PREEXISTING_READ_MAPPING"
    );
    assert!(
        kernel.names.lock().unwrap().is_empty(),
        "a mapping held only by a descendant must reject before register"
    );
    binding.binding_revision = binding.binding_revision.checked_next().unwrap();
    let text = format!(
        r#"forbid(principal, action == SecCore::Action::"file.read", resource)
        when {{ resource.path == {} || resource.path like {} }}
        unless {{ resource.path == {} }};"#,
        serde_json::to_string(root.to_str().unwrap()).unwrap(),
        serde_json::to_string(root.join("*").to_str().unwrap()).unwrap(),
        serde_json::to_string(path.to_str().unwrap()).unwrap(),
    );
    binding.policy.template = serde_json::from_value(json!({"language":"cedar", "content":{"schema":{"schemaId":"seccore.system","revision":1},"text":text}})).unwrap();
    let excluded = prepared(&client, &state, &binding);
    assert_eq!(
        client.create(&excluded).error,
        None,
        "an excluded mapping must not block apply"
    );
}

#[test]
#[ignore = "requires a disposable Lua-LSM host and root CAP_MAC_ADMIN; private targets only"]
fn live_kernel_permissions_are_independent_for_files_and_trees() {
    for tree in [false, true] {
        for denied in ["read", "write", "delete"] {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().join("tree");
            fs::create_dir(&root).unwrap();
            let path = root.join("protected");
            let outside = directory.path().join("outside");
            fs::write(&path, b"secret").unwrap();
            fs::write(&outside, b"outside").unwrap();
            let mut probe = Probe::start();
            let mut unrelated = Probe::start();
            let state = directory.path().join("state");
            let client =
                LuaLsmClient::new(Arc::new(asc_lua_lsm::NativeSecurityFs::default()), &state);
            let plan = prepared(
                &client,
                &state,
                &permission_binding(
                    if tree { &root } else { &path },
                    probe.child.id(),
                    &[denied],
                    tree,
                ),
            );
            let mut installed = Installed {
                client: &client,
                targets: vec![plan.target.clone()],
            };
            assert_eq!(client.create(&plan).error, None);
            for operation in ["read", "write", "delete"] {
                assert_eq!(unrelated.run(operation, &path), "ok", "unbound {operation}");
                fs::write(&path, b"secret").unwrap();
                let expected = if operation == denied { "errno=1" } else { "ok" };
                assert_eq!(
                    probe.run(operation, &path),
                    expected,
                    "tree={tree} deny={denied} op={operation}"
                );
                if operation == denied {
                    assert_eq!(fs::read(&path).unwrap(), b"secret");
                }
                fs::write(&path, b"secret").unwrap();
            }
            for operation in ["read", "write", "delete"] {
                assert_eq!(probe.run(operation, &outside), "ok", "outside {operation}");
            }
            assert_eq!(
                settle_native(|| client.delete(&installed.targets)).error,
                None
            );
            installed.targets.clear();
            assert_eq!(probe.run(denied, &path), "ok", "detach restores {denied}");
        }
    }
}

#[test]
#[ignore = "requires a disposable Lua-LSM host and root CAP_MAC_ADMIN; private targets only"]
fn live_kernel_reads_cover_descriptor_aliases_zero_copy_and_mapping_upgrades() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("secret");
    fs::write(&path, [b's'; 4096]).unwrap();
    let symlink = directory.path().join("symlink");
    std::os::unix::fs::symlink(&path, &symlink).unwrap();
    let mut probe = Probe::start();
    assert_eq!(probe.run("hold-rw", &path), "ok");
    let operations = [
        "readfd",
        "readv-fd",
        "preadv-fd",
        "dup-read-fd",
        "scm-read-fd",
        "sendfile-read-fd",
        "copy-read-fd",
        "splice-read-fd",
        "mmap-private-read-fd",
        "mmap-shared-read-fd",
        "mmap-private-write-fd",
        "mmap-private-exec-fd",
    ];
    for operation in operations {
        assert_eq!(probe.run(operation, &path), "ok", "baseline {operation}");
    }
    let state = directory.path().join("state");
    let client = LuaLsmClient::new(Arc::new(asc_lua_lsm::NativeSecurityFs::default()), &state);
    let plan = prepared(
        &client,
        &state,
        &permission_binding(&path, probe.child.id(), &["read"], false),
    );
    let mut installed = Installed {
        client: &client,
        targets: vec![plan.target.clone()],
    };
    assert_eq!(client.create(&plan).error, None);
    for operation in operations {
        assert_eq!(probe.run(operation, &path), "errno=1", "{operation}");
    }
    for operation in ["write-fd", "ftruncate-fd", "truncate", "otrunc"] {
        assert_eq!(
            probe.run(operation, &path),
            "ok",
            "read-only protection permits {operation}"
        );
        fs::write(&path, [b's'; 4096]).unwrap();
    }
    assert_eq!(probe.run("read", &symlink), "errno=1");
    assert_eq!(probe.run("holdmap-private-none", &path), "ok");
    assert_eq!(probe.run("mprotect-read", &path), "errno=1");
    let executable = directory.path().join("executable");
    fs::copy("/bin/true", &executable).unwrap();
    assert_eq!(probe.run("exec", &executable), "ok");
    let executable_plan = prepared(
        &client,
        &state,
        &permission_binding(&executable, probe.child.id(), &["read"], false),
    );
    installed.targets.push(executable_plan.target.clone());
    assert_eq!(client.create(&executable_plan).error, None);
    assert_eq!(probe.run("exec", &executable), "errno=1");
    assert_eq!(
        settle_native(|| client.delete(&installed.targets)).error,
        None
    );
    installed.targets.clear();
    for operation in operations {
        assert_eq!(probe.run(operation, &path), "ok", "detach {operation}");
    }
}

#[test]
#[ignore = "requires a disposable Lua-LSM host and root CAP_MAC_ADMIN; private targets only"]
fn live_kernel_writes_cover_existing_descriptors_truncation_and_mapping_upgrades() {
    let operations = [
        "write",
        "write-fd",
        "pwrite-fd",
        "writev-fd",
        "pwritev-fd",
        "truncate",
        "ftruncate-fd",
        "otrunc",
        "punch-fd",
        "copy-write-fd",
        "mmap-shared-write-fd",
        "mprotect-write",
    ];
    for tree in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("tree");
        fs::create_dir(&root).unwrap();
        let path = root.join("protected");
        let content = [b's'; 4096];
        fs::write(&path, content).unwrap();
        let mut probe = Probe::start();
        assert_eq!(probe.run("hold-rw", &path), "ok");
        assert_eq!(probe.run("holdmap-shared-read", &path), "ok");
        for operation in operations {
            assert_eq!(probe.run(operation, &path), "ok", "baseline {operation}");
            fs::write(&path, content).unwrap();
        }
        assert_eq!(probe.run("mprotect-read", &path), "ok");
        let state = directory.path().join("state");
        let client = LuaLsmClient::new(Arc::new(asc_lua_lsm::NativeSecurityFs::default()), &state);
        let plan = prepared(
            &client,
            &state,
            &permission_binding(
                if tree { &root } else { &path },
                probe.child.id(),
                &["write"],
                tree,
            ),
        );
        let mut installed = Installed {
            client: &client,
            targets: vec![plan.target.clone()],
        };
        assert_eq!(client.create(&plan).error, None);
        for operation in operations {
            assert_eq!(
                probe.run(operation, &path),
                "errno=1",
                "tree={tree} {operation}"
            );
            assert_eq!(
                fs::read(&path).unwrap(),
                content,
                "{operation} must preserve content"
            );
        }
        assert_eq!(probe.run("mmap-private-write-fd", &path), "ok");
        assert_eq!(
            fs::read(&path).unwrap(),
            content,
            "private COW must not change the file"
        );
        assert_eq!(probe.run("readfd", &path), "ok");
        assert_eq!(probe.run("mprotect-read", &path), "ok");
        assert_eq!(probe.run("holdmap-private-read", &path), "ok");
        assert_eq!(
            probe.run("mprotect-write", &path),
            "ok",
            "private mapping upgrades do not write the backing file"
        );
        assert_eq!(probe.run("chmod", &path), "ok");
        assert_eq!(
            settle_native(|| client.delete(&installed.targets)).error,
            None
        );
        installed.targets.clear();
        assert_eq!(probe.run("pwrite-fd", &path), "ok");
    }
}

#[test]
#[ignore = "requires a disposable Lua-LSM host and root CAP_MAC_ADMIN; private targets only"]
fn live_kernel_delete_rename_exchange_and_ancestor_stability() {
    let directory = tempfile::tempdir().unwrap();
    let ancestor = directory.path().join("parent");
    let root = ancestor.join("tree");
    let outside = directory.path().join("outside");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("entry");
    let destination = root.join("renamed");
    let empty = root.join("empty");
    fs::create_dir(&empty).unwrap();
    fs::write(&path, b"secret").unwrap();
    fs::write(&outside, b"outside").unwrap();
    let mut probe = Probe::start();
    let state = directory.path().join("state");
    let client = LuaLsmClient::new(Arc::new(asc_lua_lsm::NativeSecurityFs::default()), &state);
    let plan = prepared(
        &client,
        &state,
        &permission_binding(&root, probe.child.id(), &["delete"], true),
    );
    let mut installed = Installed {
        client: &client,
        targets: vec![plan.target.clone()],
    };
    assert_eq!(client.create(&plan).error, None);
    for (operation, source) in [
        ("delete".into(), &path),
        ("rmdir".into(), &empty),
        (format!("rename:{}", outside.display()), &path),
        (format!("rename:{}", path.display()), &outside),
        (format!("exchange:{}", path.display()), &outside),
        (format!("exchange:{}", outside.display()), &path),
        (
            format!("rename:{}", directory.path().join("moved-tree").display()),
            &root,
        ),
        (
            format!("rename:{}", directory.path().join("moved-parent").display()),
            &ancestor,
        ),
    ] {
        assert_eq!(probe.run(&operation, source), "errno=1", "{operation}");
        assert!(
            source.exists(),
            "denied namespace change must preserve source"
        );
        assert_eq!(fs::read(&path).unwrap(), b"secret");
        assert_eq!(fs::read(&outside).unwrap(), b"outside");
    }
    assert_eq!(
        probe.run(&format!("rename:{}", destination.display()), &path),
        "ok"
    );
    assert_eq!(probe.run("delete", &destination), "errno=1");
    assert_eq!(
        probe.run("write", &destination),
        "ok",
        "delete-only must allow content writes"
    );
    assert_eq!(
        settle_native(|| client.delete(&installed.targets)).error,
        None
    );
    installed.targets.clear();
    assert_eq!(probe.run("delete", &destination), "ok");
    assert_eq!(probe.run("rmdir", &empty), "ok");
}

#[test]
#[ignore = "requires a disposable Lua-LSM host and root CAP_MAC_ADMIN; private targets only"]
fn live_kernel_mapping_preflight_and_failed_update_keep_old_protection() {
    for (operation, mapping, code) in [
        (
            "read",
            "holdmap-private-read",
            "LUA_LSM_PREEXISTING_READ_MAPPING",
        ),
        (
            "read",
            "holdmap-private-none",
            "LUA_LSM_PREEXISTING_READ_MAPPING",
        ),
        (
            "read",
            "holdmap-shared-read",
            "LUA_LSM_PREEXISTING_READ_MAPPING",
        ),
        (
            "write",
            "holdmap-shared-write",
            "LUA_LSM_PREEXISTING_WRITABLE_MAPPING",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("protected");
        fs::write(&path, [b's'; 4096]).unwrap();
        let mut probe = Probe::start();
        assert_eq!(probe.run(mapping, &path), "ok");
        let state = directory.path().join("state");
        let client = LuaLsmClient::new(Arc::new(asc_lua_lsm::NativeSecurityFs::default()), &state);
        let plan = prepared(
            &client,
            &state,
            &permission_binding(&path, probe.child.id(), &[operation], false),
        );
        let installed = Installed {
            client: &client,
            targets: vec![plan.target.clone()],
        };
        let report = client.create(&plan);
        assert_eq!(report.error.unwrap().code, code);
        assert_eq!(report.observations[0].presence, Presence::Unknown);
        drop(installed);
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("protected");
    fs::write(&path, [b's'; 4096]).unwrap();
    let mut probe = Probe::start();
    let state = directory.path().join("state");
    let client = LuaLsmClient::new(Arc::new(asc_lua_lsm::NativeSecurityFs::default()), &state);
    let old_binding = permission_binding(&path, probe.child.id(), &["write"], false);
    let old = prepared(&client, &state, &old_binding);
    let mut installed = Installed {
        client: &client,
        targets: vec![old.target.clone()],
    };
    assert_eq!(client.create(&old).error, None);
    assert_eq!(probe.run("holdmap-private-read", &path), "ok");
    let mut next_binding = permission_binding(&path, probe.child.id(), &["read"], false);
    next_binding.binding_id = old_binding.binding_id;
    next_binding.binding_revision = old_binding.binding_revision.checked_next().unwrap();
    let next = prepared(&client, &state, &next_binding);
    installed.targets.push(next.target.clone());
    let report = client.update(std::slice::from_ref(&old.target), &next);
    assert_eq!(
        report.error.unwrap().code,
        "LUA_LSM_PREEXISTING_READ_MAPPING"
    );
    assert_eq!(
        probe.run("write", &path),
        "errno=1",
        "old protection survives failed update"
    );
    assert_eq!(
        probe.run("read", &path),
        "ok",
        "new read protection is not reported active"
    );
    assert_eq!(probe.run("unmap", &path), "ok");
    assert_eq!(
        settle_native(|| client.update(std::slice::from_ref(&old.target), &next)).error,
        None
    );
    assert_eq!(probe.run("read", &path), "errno=1");
    assert_eq!(
        probe.run("write", &path),
        "ok",
        "successful retry replaces the old write policy"
    );
    assert_eq!(
        settle_native(|| client.delete(&installed.targets)).error,
        None
    );
    installed.targets.clear();
}

#[test]
#[ignore = "requires a disposable Lua-LSM host and root CAP_MAC_ADMIN; private targets only"]
fn live_kernel_existing_children_threads_exec_and_setsid_keep_subject_membership() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("protected");
    fs::write(&path, b"secret").unwrap();
    let mut probe = Probe::start();
    assert_eq!(probe.run("spawn-child", &path), "ok");
    for operation in [
        "existing-child-read",
        "thread-read",
        "exec-child-read",
        "setsid-child-read",
    ] {
        assert_eq!(probe.run(operation, &path), "ok", "baseline {operation}");
    }
    let state = directory.path().join("state");
    let client = LuaLsmClient::new(Arc::new(asc_lua_lsm::NativeSecurityFs::default()), &state);
    let plan = prepared(
        &client,
        &state,
        &permission_binding(&path, probe.child.id(), &["read"], false),
    );
    let mut installed = Installed {
        client: &client,
        targets: vec![plan.target.clone()],
    };
    assert_eq!(client.create(&plan).error, None);
    for operation in [
        "existing-child-read",
        "thread-read",
        "exec-child-read",
        "setsid-child-read",
    ] {
        assert_eq!(probe.run(operation, &path), "errno=1", "{operation}");
    }
    assert_eq!(
        settle_native(|| client.delete(&installed.targets)).error,
        None
    );
    installed.targets.clear();
    assert_eq!(probe.run("existing-child-read", &path), "ok");
}

#[test]
#[ignore = "requires a disposable Lua-LSM host and root CAP_MAC_ADMIN; private targets only"]
fn live_kernel_uid_changes_preserve_protection_and_unprivileged_subject_cannot_unload() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let path = directory.path().join("protected");
    fs::write(&path, b"secret").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
    let mut probe = Probe::start();
    let mut unrelated = Probe::start();
    let state = directory.path().join("state");
    let client = LuaLsmClient::new(Arc::new(asc_lua_lsm::NativeSecurityFs::default()), &state);
    let plan = prepared(
        &client,
        &state,
        &permission_binding(&path, probe.child.id(), &["read"], false),
    );
    let mut installed = Installed {
        client: &client,
        targets: vec![plan.target.clone()],
    };
    assert_eq!(client.create(&plan).error, None);
    assert_eq!(probe.run("drop-uid", &path), "ok");
    assert_eq!(unrelated.run("drop-uid", &path), "ok");
    assert_eq!(
        unrelated.run("read", &path),
        "ok",
        "DAC must permit the unbound control"
    );
    assert_eq!(probe.run("read", &path), "errno=1");
    let tamper = probe.run("unregister", Path::new(&plan.target.id));
    assert!(
        matches!(tamper.as_str(), "errno=1" | "errno=13"),
        "{tamper}"
    );
    assert_eq!(
        probe.run("read", &path),
        "errno=1",
        "unload attempt must leave protection active"
    );
    let seed = state
        .join(&plan.target.id)
        .join("seed")
        .join(probe.child.id().to_string());
    assert_eq!(probe.run("read", &seed), "errno=13");
    assert_eq!(
        settle_native(|| client.delete(&installed.targets)).error,
        None
    );
    installed.targets.clear();
    assert_eq!(probe.run("read", &path), "ok");
}

#[test]
#[ignore = "requires a disposable Lua-LSM host and root CAP_MAC_ADMIN; private targets only"]
fn live_kernel_overlapping_bindings_keep_independent_permissions_and_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("protected");
    fs::write(&path, b"secret").unwrap();
    let mut probe = Probe::start();
    let state = directory.path().join("state");
    let client = LuaLsmClient::new(Arc::new(asc_lua_lsm::NativeSecurityFs::default()), &state);
    let read = prepared(
        &client,
        &state,
        &permission_binding(&path, probe.child.id(), &["read"], false),
    );
    let write = prepared(
        &client,
        &state,
        &permission_binding(&path, probe.child.id(), &["write"], false),
    );
    let mut installed = Installed {
        client: &client,
        targets: vec![read.target.clone(), write.target.clone()],
    };
    assert_eq!(client.create(&read).error, None);
    assert_eq!(client.create(&write).error, None);
    assert_eq!(probe.run("read", &path), "errno=1");
    assert_eq!(probe.run("write", &path), "errno=1");
    assert_eq!(
        settle_native(|| client.delete(std::slice::from_ref(&read.target))).error,
        None
    );
    assert_eq!(probe.run("read", &path), "ok");
    assert_eq!(
        probe.run("write", &path),
        "errno=1",
        "deleting the read Binding must preserve the write Binding"
    );
    assert_eq!(client.create(&read).error, None);
    assert_eq!(
        settle_native(|| client.delete(std::slice::from_ref(&write.target))).error,
        None
    );
    assert_eq!(
        probe.run("read", &path),
        "errno=1",
        "deleting the write Binding must preserve the read Binding"
    );
    assert_eq!(probe.run("write", &path), "ok");
    assert_eq!(
        settle_native(|| client.delete(&installed.targets)).error,
        None
    );
    installed.targets.clear();
    assert_eq!(probe.run("read", &path), "ok");
    assert_eq!(probe.run("write", &path), "ok");
}

#[test]
#[ignore = "requires root, Lua-LSM and unshare; mounts stay in a private PID/mount namespace"]
fn live_kernel_pid_namespace_bind_aliases_and_mount_guards() {
    let directory = tempfile::tempdir().unwrap();
    let tree = directory.path().join("tree");
    let alias = directory.path().join("alias");
    let unrelated_source = directory.path().join("unrelated-source");
    let unrelated_destination = directory.path().join("unrelated-destination");
    for path in [&tree, &alias, &unrelated_source, &unrelated_destination] {
        fs::create_dir(path).unwrap();
    }
    let file = tree.join("protected");
    let via_alias = alias.join("protected");
    fs::write(&file, b"secret").unwrap();
    let mut probe = Probe::with_namespaces(&[
        "--mount",
        "--pid",
        "--fork",
        "--mount-proc",
        "--propagation",
        "private",
    ]);
    assert_eq!(probe.run("read", &file), "ok", "namespace subject is ready");
    assert_eq!(
        probe.run(&format!("bind-mount:{}", alias.display()), &tree),
        "ok"
    );
    assert_eq!(probe.run("read", &via_alias), "ok");
    assert!(
        !via_alias.exists(),
        "the subject's private mount must not change the host view"
    );
    let mut unrelated = Probe::start();
    let state = directory.path().join("state");
    let client = LuaLsmClient::new(Arc::new(asc_lua_lsm::NativeSecurityFs::default()), &state);
    let plan = prepared(
        &client,
        &state,
        &permission_binding(&tree, probe.child.id(), &["read", "write", "delete"], true),
    );
    let mut installed = Installed {
        client: &client,
        targets: vec![plan.target.clone()],
    };
    assert_eq!(client.create(&plan).error, None);
    for path in [&file, &via_alias] {
        for operation in ["read", "write", "delete"] {
            assert_eq!(
                probe.run(operation, path),
                "errno=1",
                "{operation} {}",
                path.display()
            );
        }
    }
    assert_eq!(unrelated.run("read", &file), "ok");
    assert_eq!(
        probe.run(&format!("bind-mount:{}", tree.display()), &unrelated_source),
        "errno=1"
    );
    assert_eq!(
        probe.run(
            &format!("bind-mount:{}", unrelated_destination.display()),
            &unrelated_source
        ),
        "ok"
    );
    assert_eq!(fs::read(&file).unwrap(), b"secret");
    assert_eq!(
        settle_native(|| client.delete(&installed.targets)).error,
        None
    );
    installed.targets.clear();
    assert_eq!(probe.run("read", &via_alias), "ok");
    assert!(!via_alias.exists());
}

fn pattern_binding(path: &Path, pid: u32, operation: &str, cedar: bool) -> PreparedBinding {
    let mut binding = permission_binding(path, pid, &[operation], false);
    if cedar {
        let asc_policy_types::authoring::PolicySource::Cedar(source) = &mut binding.policy.template
        else {
            unreachable!()
        };
        source.text = source
            .text
            .replace("resource.path == ", "resource.path like ");
    } else {
        let mut rule = json!({"effect":"block","category":"file",
            "action":if operation == "read" {"read"} else {"write"},
            "target":{"type":"file","path":path}});
        if operation != "read" {
            rule["where"] = json!({"operation":{"eq":operation}});
        }
        binding.policy.template = serde_json::from_value(
            json!({"language":"seccore","content":{"specVersion":"0.1","rules":[rule]}}),
        )
        .unwrap();
    }
    binding
}

#[test]
fn both_pattern_languages_reach_preparation_with_literal_scan_roots() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let client = LuaLsmClient::new(Arc::new(Kernel::default()), &state);
    for cedar in [true, false] {
        let pattern = dir.path().join("*.txt");
        let prepared = prepared(
            &client,
            &state,
            &pattern_binding(&pattern, std::process::id(), "read", cedar),
        );
        let decoded: LuaLsmPlan = serde_json::from_slice(&prepared.content).unwrap();
        assert_eq!(decoded.policy.targets[0].path, dir.path().to_str().unwrap());
        assert_eq!(decoded.patterns[&0][0].value, pattern.to_str().unwrap());
        assert!(decoded.source.contains("local function pattern_matches"));
    }
}

#[test]
fn cedar_scan_scope_uses_all_conditions_in_each_branch() {
    let mut binding = binding(Path::new("/work"));
    let state = Path::new("/private-state");
    let client = LuaLsmClient::new(Arc::new(Kernel::default()), state);
    for (condition, expected) in [
        (
            r#"resource.path like "*.txt" && resource.path like "/work/*""#,
            Some("/work"),
        ),
        (
            r#"resource.path == "/work/a?.txt" && !(resource.path like "*tmp*")"#,
            Some("/work/a?.txt"),
        ),
        (
            r#"resource.path like "/work/*" && resource.path like "/work/sub/*""#,
            Some("/work/sub"),
        ),
        (
            r#"resource.path like "/work/*" || resource.path like "*.txt""#,
            None,
        ),
        (r#"!(resource.path like "/work/*")"#, None),
    ] {
        let asc_policy_types::authoring::PolicySource::Cedar(source) = &mut binding.policy.template
        else {
            unreachable!()
        };
        source.text = format!(
            r#"forbid(principal, action == SecCore::Action::"file.read", resource) when {{ {condition} }};"#
        );
        if let Some(root) = expected {
            let plan = prepared(&client, state, &binding);
            let decoded: LuaLsmPlan = serde_json::from_slice(&plan.content).unwrap();
            assert_eq!(decoded.policy.targets[0].path, root);
        } else {
            assert!(
                matches!(LuaLsmTranslator::new(state).translate(&binding).unwrap(),
               TranslationOutcome::Rejected(error) if error.code == "CEDAR_RULE_0_UNBOUNDED_PATH_SCOPE")
            );
        }
    }
}

#[test]
#[ignore = "requires a disposable Lua-LSM host and root CAP_MAC_ADMIN"]
fn live_kernel_glob_and_cedar_like_permissions() {
    for (cedar, pattern, nested_matches) in [
        (false, "s?.txt", false),
        (false, "**/s?.txt", true),
        (true, "s*.txt", true),
    ] {
        for op in ["read", "write", "delete"] {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().join("files");
            fs::create_dir_all(root.join("sub")).unwrap();
            let path = root.join("s1.txt");
            let alias = directory.path().join("outside");
            fs::write(&path, b"data").unwrap();
            fs::hard_link(&path, &alias).unwrap();
            let nested = root.join("sub/s2.txt");
            fs::write(&nested, b"data").unwrap();
            let other = root.join("other.txt");
            fs::write(&other, b"data").unwrap();
            let mut probe = Probe::start();
            let state = directory.path().join("state");
            let client =
                LuaLsmClient::new(Arc::new(asc_lua_lsm::NativeSecurityFs::default()), &state);
            let plan = prepared(
                &client,
                &state,
                &pattern_binding(&root.join(pattern), probe.child.id(), op, cedar),
            );
            let _installed = Installed {
                client: &client,
                targets: vec![plan.target.clone()],
            };
            assert_eq!(client.create(&plan).error, None);
            assert_eq!(probe.run(op, &path), "errno=1", "{op} {pattern}");
            assert_eq!(probe.run(op, &alias), "errno=1");
            assert_eq!(
                probe.run(op, &nested),
                if nested_matches { "errno=1" } else { "ok" }
            );
            assert_eq!(probe.run(op, &other), "ok");
            let incoming = root.join("incoming.log");
            fs::write(&incoming, b"incoming").unwrap();
            let link = root.join("s5.txt");
            let add_link = format!("link:{}", link.display());
            if op == "delete" {
                assert_eq!(probe.run(&add_link, &incoming), "ok");
                assert_eq!(probe.run("delete", &incoming), "errno=1");
            } else {
                assert_eq!(probe.run(&add_link, &incoming), "errno=1");
                assert_eq!(
                    probe.run(&format!("rename:{}", link.display()), &incoming),
                    "errno=1"
                );
            }
            assert_eq!(
                probe.run(
                    &format!("rename:{}", root.join("relocated").display()),
                    &root.join("sub")
                ),
                "errno=1"
            );
            let created = root.join("s3.txt");
            fs::write(&created, b"new").unwrap();
            assert_eq!(probe.run(op, &created), "errno=1");
            let late = root.join("sub-new");
            fs::create_dir(&late).unwrap();
            let late = late.join("s4.txt");
            fs::write(&late, b"new").unwrap();
            assert_eq!(
                probe.run(op, &late),
                if nested_matches { "errno=1" } else { "ok" }
            );
        }
    }
}

#[test]
#[ignore = "requires a disposable Lua-LSM host and root CAP_MAC_ADMIN"]
fn live_kernel_pattern_preflight_uses_lua_matches_and_inode_identity() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("files");
    fs::create_dir(&root).unwrap();
    let protected = root.join("secret.txt");
    let alias = dir.path().join("alias");
    fs::write(&protected, vec![0u8; 4096]).unwrap();
    fs::hard_link(&protected, &alias).unwrap();
    let unrelated = root.join("ignored.log");
    fs::write(&unrelated, vec![0u8; 4096]).unwrap();
    let mut probe = Probe::start();
    let state = dir.path().join("state");
    let client = LuaLsmClient::new(Arc::new(asc_lua_lsm::NativeSecurityFs::default()), &state);
    let plan = prepared(
        &client,
        &state,
        &pattern_binding(&root.join("*.txt"), probe.child.id(), "read", true),
    );
    let _installed = Installed {
        client: &client,
        targets: vec![plan.target.clone()],
    };
    assert_eq!(probe.run("holdmap-private-read", &alias), "ok");
    assert_eq!(
        client.create(&plan).error.unwrap().code,
        "LUA_LSM_PREEXISTING_READ_MAPPING"
    );
    assert_eq!(probe.run("unmap", &alias), "ok");
    assert_eq!(probe.run("holdmap-private-read", &unrelated), "ok");
    assert_eq!(client.create(&plan).error, None);
    assert_eq!(probe.run("read", &alias), "errno=1");
}

#[test]
#[ignore = "requires a disposable Lua-LSM host and root CAP_MAC_ADMIN"]
fn live_kernel_like_keeps_literal_wildcards_and_exclusions() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    fs::create_dir_all(root.join("nested")).unwrap();
    let mut probe = Probe::start();
    let mut binding = permission_binding(&root, probe.child.id(), &["read"], true);
    let asc_policy_types::authoring::PolicySource::Cedar(source) = &mut binding.policy.template
    else {
        unreachable!()
    };
    let base = root.display();
    source.text = format!(
        r#"forbid(principal, action == SecCore::Action::"file.read", resource)
        when {{ resource.path like "{base}/*" && resource.path like "*?\*.txt" }}
        unless {{ resource.path like "{base}/public*" }};
        forbid(principal, action == SecCore::Action::"file.read", resource)
        when {{ resource.path == "{base}/literal?.bin" && !(resource.path like "*.tmp") }};"#
    );
    fs::write(root.join("literal?.bin"), b"literal").unwrap();
    let state = dir.path().join("state");
    let client = LuaLsmClient::new(Arc::new(asc_lua_lsm::NativeSecurityFs::default()), &state);
    let plan = prepared(&client, &state, &binding);
    let _installed = Installed {
        client: &client,
        targets: vec![plan.target.clone()],
    };
    assert_eq!(client.create(&plan).error, None);
    for (name, expected) in [
        ("nested/file?*.txt", "errno=1"),
        ("public?*.txt", "ok"),
        ("literal?.bin", "errno=1"),
        ("literalX.bin", "ok"),
        ("plain.txt", "ok"),
    ] {
        let path = root.join(name);
        fs::write(&path, b"new").unwrap();
        assert_eq!(probe.run("read", &path), expected);
    }
}
