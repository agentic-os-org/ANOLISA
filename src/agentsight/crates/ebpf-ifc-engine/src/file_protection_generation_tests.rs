use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use super::*;
use crate::{FileProtectionDomainValue, FILE_PROTECTION_MAP_CAPACITY};

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
