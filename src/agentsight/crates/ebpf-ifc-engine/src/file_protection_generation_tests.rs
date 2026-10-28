use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use super::*;
use crate::{FileProtectionDomainValue, TreeGuard, FILE_PROTECTION_MAP_CAPACITY};

#[derive(Debug, Default)]
struct State {
    operations: Vec<&'static str>,
    roots: HashSet<(TreePathIdentity, u32, u32)>,
    directories: HashSet<(TreePathIdentity, u32, u32)>,
    stable: HashSet<(TreePathIdentity, u32, u32)>,
    slots: HashSet<(FileProtectionSlot, u32, u32)>,
    targets: HashSet<(u32, u32, u32)>,
    active: HashMap<u32, u32>,
    fail_once: Option<&'static str>,
    capacities: HashMap<&'static str, (usize, usize)>,
}

#[derive(Clone, Debug, Default)]
struct FakeWriter(Arc<Mutex<State>>);

impl FakeWriter {
    fn fail_once(&self, operation: &'static str) {
        self.0.lock().expect("state lock").fail_once = Some(operation);
    }

    fn run(&self, operation: &'static str) -> io::Result<()> {
        let mut state = self.0.lock().expect("state lock");
        state.operations.push(operation);
        if state.fail_once == Some(operation) {
            state.fail_once = None;
            Err(io::Error::other(format!("{operation} failed")))
        } else {
            Ok(())
        }
    }
}

impl FileProtectionMapWriter for FakeWriter {
    fn usage(&mut self, map: &'static str) -> io::Result<(usize, usize)> {
        Ok(self
            .0
            .lock()
            .expect("state lock")
            .capacities
            .get(map)
            .copied()
            .unwrap_or((0, FILE_PROTECTION_MAP_CAPACITY)))
    }

    fn insert_root(
        &mut self,
        key: TreePathIdentity,
        domain: u32,
        generation: FileProtectionGeneration,
        _mask: u64,
    ) -> io::Result<()> {
        self.run("insert_root")?;
        self.0
            .lock()
            .expect("state lock")
            .roots
            .insert((key, domain, generation.get()));
        Ok(())
    }

    fn remove_root(
        &mut self,
        key: TreePathIdentity,
        domain: u32,
        generation: FileProtectionGeneration,
    ) -> io::Result<()> {
        self.run("remove_root")?;
        self.0
            .lock()
            .expect("state lock")
            .roots
            .remove(&(key, domain, generation.get()));
        Ok(())
    }

    fn insert_directory(
        &mut self,
        key: TreePathIdentity,
        domain: u32,
        generation: FileProtectionGeneration,
        _mask: u64,
    ) -> io::Result<()> {
        self.run("insert_directory")?;
        self.0
            .lock()
            .expect("state lock")
            .directories
            .insert((key, domain, generation.get()));
        Ok(())
    }

    fn remove_directory(
        &mut self,
        key: TreePathIdentity,
        domain: u32,
        generation: FileProtectionGeneration,
    ) -> io::Result<()> {
        self.run("remove_directory")?;
        self.0
            .lock()
            .expect("state lock")
            .directories
            .remove(&(key, domain, generation.get()));
        Ok(())
    }

    fn insert_stable_path(
        &mut self,
        key: TreePathIdentity,
        domain: u32,
        generation: FileProtectionGeneration,
        _mask: u64,
    ) -> io::Result<()> {
        self.run("insert_stable")?;
        self.0
            .lock()
            .expect("state lock")
            .stable
            .insert((key, domain, generation.get()));
        Ok(())
    }

    fn remove_stable_path(
        &mut self,
        key: TreePathIdentity,
        domain: u32,
        generation: FileProtectionGeneration,
    ) -> io::Result<()> {
        self.run("remove_stable")?;
        self.0
            .lock()
            .expect("state lock")
            .stable
            .remove(&(key, domain, generation.get()));
        Ok(())
    }

    fn insert_slot(
        &mut self,
        key: FileProtectionSlot,
        domain: u32,
        generation: FileProtectionGeneration,
        _value: FileProtectionSlotValue,
    ) -> io::Result<()> {
        self.run("insert_slot")?;
        self.0
            .lock()
            .expect("state lock")
            .slots
            .insert((key, domain, generation.get()));
        Ok(())
    }

    fn remove_slot(
        &mut self,
        key: FileProtectionSlot,
        domain: u32,
        generation: FileProtectionGeneration,
    ) -> io::Result<()> {
        self.run("remove_slot")?;
        self.0
            .lock()
            .expect("state lock")
            .slots
            .remove(&(key, domain, generation.get()));
        Ok(())
    }

    fn insert_target(
        &mut self,
        domain: u32,
        generation: FileProtectionGeneration,
        value: FileProtectionTargetValue,
    ) -> io::Result<()> {
        self.run("insert_target")?;
        self.0.lock().expect("state lock").targets.insert((
            domain,
            generation.get(),
            value.target_id,
        ));
        Ok(())
    }

    fn remove_target(
        &mut self,
        domain: u32,
        generation: FileProtectionGeneration,
        target_id: u32,
    ) -> io::Result<()> {
        self.run("remove_target")?;
        self.0
            .lock()
            .expect("state lock")
            .targets
            .remove(&(domain, generation.get(), target_id));
        Ok(())
    }

    fn activate(
        &mut self,
        domain: u32,
        generation: FileProtectionGeneration,
        _value: FileProtectionDomainValue,
    ) -> io::Result<()> {
        self.run("activate")?;
        let mut state = self.0.lock().expect("state lock");
        if state.active.contains_key(&domain) {
            return Err(io::Error::other("domain already active"));
        }
        state.active.insert(domain, generation.get());
        Ok(())
    }

    fn deactivate(&mut self, domain: u32, generation: FileProtectionGeneration) -> io::Result<()> {
        self.run("deactivate")?;
        let mut state = self.0.lock().expect("state lock");
        if state.active.get(&domain) == Some(&generation.get()) {
            state.active.remove(&domain);
        }
        Ok(())
    }
}

fn identity(ino: u64) -> TreePathIdentity {
    TreePathIdentity {
        ino,
        dev: 8,
        mnt_id: 3,
    }
}

fn generation(value: u32) -> FileProtectionGeneration {
    FileProtectionGeneration::new(value).expect("nonzero generation")
}

fn slot() -> FileProtectionSlot {
    FileProtectionSlot::new(identity(2), "entry").expect("valid slot")
}

fn counts() -> FileProtectionMapCounts {
    FileProtectionMapCounts {
        roots: 1,
        directories: 1,
        stable_paths: 1,
        slots: 1,
        targets: 1,
        domains: 1,
    }
}

fn guard(target_id: u32) -> TreeGuard {
    TreeGuard {
        flags: 3,
        rule_id: target_id,
    }
}

fn target(target_id: u32) -> FileProtectionTargetValue {
    FileProtectionTargetValue {
        flags: 3,
        target_id,
        guard: guard(target_id),
    }
}

fn domain() -> FileProtectionDomainValue {
    FileProtectionDomainValue {
        target_count: 1,
        tree_mask: 1,
        delete_mask: 1,
        ordinary_write_mask: 1,
        protection_set_id: 77,
        guard: guard(1),
    }
}

fn stage_all(writer: FakeWriter, generation_id: u32) -> FileProtectionStaging<FakeWriter> {
    let mut staging = FileProtectionStaging::begin(writer, 9, generation(generation_id), counts())
        .expect("begin transaction");
    staging.stage_root(identity(1), 1).expect("stage root");
    staging
        .stage_directory(identity(1), 1)
        .expect("stage directory");
    staging
        .stage_stable_path(identity(2), 1)
        .expect("stage stable");
    staging
        .stage_slot(
            slot(),
            FileProtectionSlotValue {
                direct_mask: 1,
                exact_exclude_mask: 0,
                subtree_exclude_mask: 0,
            },
        )
        .expect("stage slot");
    staging.stage_target(target(1)).expect("stage target");
    staging
}

#[test]
fn staging_failure_rolls_back_only_the_staged_generation() {
    let writer = FakeWriter::default();
    writer
        .0
        .lock()
        .expect("state lock")
        .roots
        .insert((identity(99), 9, 8));
    writer.fail_once("insert_slot");
    let mut staging =
        FileProtectionStaging::begin(writer.clone(), 9, generation(2), counts()).expect("begin");
    staging.stage_root(identity(1), 1).expect("root");
    staging.stage_directory(identity(1), 1).expect("directory");
    staging.stage_stable_path(identity(2), 1).expect("stable");
    assert!(staging
        .stage_slot(
            slot(),
            FileProtectionSlotValue {
                direct_mask: 1,
                ..Default::default()
            }
        )
        .is_err());
    let state = writer.0.lock().expect("state lock");
    assert!(state.roots.contains(&(identity(99), 9, 8)));
    assert!(!state.roots.contains(&(identity(1), 9, 2)));
}

#[test]
fn capacities_target_limit_and_slot_boundaries_are_validated() {
    let writer = FakeWriter::default();
    writer
        .0
        .lock()
        .expect("state lock")
        .capacities
        .insert("te_policy_slots", (4095, 4096));
    let error = FileProtectionStaging::begin(
        writer,
        9,
        generation(5),
        FileProtectionMapCounts {
            slots: 2,
            ..Default::default()
        },
    )
    .expect_err("capacity must fail");
    assert!(error
        .to_string()
        .contains("4095 existing + 2 staged > 4096"));
    assert!(FileProtectionGeneration::new(0).is_err());
    assert!(FileProtectionSlot::new(identity(1), "").is_err());
    assert!(FileProtectionSlot::new(identity(1), &"x".repeat(256)).is_err());
    assert!(FileProtectionMapCounts {
        targets: 65,
        ..Default::default()
    }
    .validate()
    .is_err());
}

#[test]
fn activation_is_last_and_detach_deactivation_is_first() {
    let writer = FakeWriter::default();
    let active = stage_all(writer.clone(), 1)
        .activate(domain())
        .expect("activate transaction");
    assert_eq!(
        writer.0.lock().expect("state lock").operations.last(),
        Some(&"activate")
    );
    active.detach().expect("detach transaction");
    let operations = &writer.0.lock().expect("state lock").operations;
    let deactivate = operations
        .iter()
        .position(|operation| *operation == "deactivate")
        .expect("deactivate operation");
    assert_eq!(operations[deactivate + 1], "remove_target");
}

#[test]
fn activation_failure_rolls_back_only_the_staged_generation() {
    let writer = FakeWriter::default();
    writer
        .0
        .lock()
        .expect("state lock")
        .roots
        .insert((identity(99), 9, 8));

    writer.fail_once("activate");
    assert!(stage_all(writer.clone(), 3).activate(domain()).is_err());

    let state = writer.0.lock().expect("state lock");
    assert!(state.roots.contains(&(identity(99), 9, 8)));
    assert!(!state
        .roots
        .iter()
        .any(|(_, _, generation)| *generation == 3));
    assert!(!state.active.contains_key(&9));
}

#[test]
fn partial_cleanup_retains_retry_metadata_and_retry_is_idempotent() {
    let writer = FakeWriter::default();
    let active = stage_all(writer.clone(), 4)
        .activate(domain())
        .expect("activate");
    writer.fail_once("remove_slot");
    let failure = active.detach().expect_err("partial cleanup must surface");
    assert!(!writer.0.lock().expect("state lock").active.contains_key(&9));
    assert!(writer
        .0
        .lock()
        .expect("state lock")
        .slots
        .contains(&(slot(), 9, 4)));
    failure.retry.retry_cleanup().expect("retry cleanup");
    let state = writer.0.lock().expect("state lock");
    assert!(state.roots.is_empty() && state.directories.is_empty() && state.stable.is_empty());
    assert!(state.slots.is_empty() && state.targets.is_empty());
}

#[test]
fn failed_deactivation_preserves_active_maps_until_retry() {
    let writer = FakeWriter::default();
    let active = stage_all(writer.clone(), 6)
        .activate(domain())
        .expect("activate");
    writer.fail_once("deactivate");
    let failure = active.detach().expect_err("deactivation must surface");
    let state = writer.0.lock().expect("state lock");
    assert_eq!(state.active.get(&9), Some(&6));
    assert!(state.roots.contains(&(identity(1), 9, 6)));
    drop(state);
    failure
        .retry
        .retry_cleanup()
        .expect("deactivate and cleanup retry");
    assert!(!writer.0.lock().expect("state lock").active.contains_key(&9));
}

#[test]
fn active_drop_deactivates_before_cleanup() {
    let writer = FakeWriter::default();
    let active = stage_all(writer.clone(), 7)
        .activate(domain())
        .expect("activate");
    drop(active);
    let state = writer.0.lock().expect("state lock");
    let deactivate = state
        .operations
        .iter()
        .position(|op| *op == "deactivate")
        .expect("deactivate");
    assert_eq!(state.operations[deactivate + 1], "remove_target");
    assert!(state.active.is_empty() && state.roots.is_empty() && state.slots.is_empty());
}

#[test]
fn failed_active_drop_preserves_all_generation_entries() {
    let writer = FakeWriter::default();
    let active = stage_all(writer.clone(), 8)
        .activate(domain())
        .expect("activate");
    writer.fail_once("deactivate");
    drop(active);
    let state = writer.0.lock().expect("state lock");
    assert_eq!(state.active.get(&9), Some(&8));
    assert!(state.roots.contains(&(identity(1), 9, 8)));
    assert!(state.directories.contains(&(identity(1), 9, 8)));
    assert!(state.stable.contains(&(identity(2), 9, 8)));
    assert!(state.slots.contains(&(slot(), 9, 8)));
    assert!(state.targets.contains(&(9, 8, 1)));
    assert_eq!(state.operations.last(), Some(&"deactivate"));
}

#[test]
fn failed_detach_retry_drop_keeps_maps_until_deactivation() {
    let writer = FakeWriter::default();
    let active = stage_all(writer.clone(), 9)
        .activate(domain())
        .expect("activate");
    writer.fail_once("deactivate");
    let failure = active.detach().expect_err("deactivation fails");
    drop(failure.retry);
    let state = writer.0.lock().expect("state lock");
    assert_eq!(state.active.get(&9), Some(&9));
    assert!(state.slots.contains(&(slot(), 9, 9)));
    assert_eq!(state.operations.last(), Some(&"deactivate"));
}
