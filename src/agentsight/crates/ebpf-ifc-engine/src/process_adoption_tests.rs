//! 收养事务的确定性故障注入，不操作主机 BPF maps。

use super::*;
use std::collections::BTreeMap;

type Key = (u8, i32, u32, u64);
fn key(entry: Entry) -> Key {
    match entry {
        Entry::Global(pid, _) => (0, pid, 0, 0),
        Entry::Domain(pid, domain, _) => (1, pid, domain, 0),
        Entry::Root(pid, _) => (2, pid, 0, 0),
        Entry::Provenance(pid, domain, label, _) => (3, pid, domain, label),
        Entry::Task(pid, _) => (4, pid, 0, 0),
    }
}
struct Fake {
    entries: BTreeMap<Key, Entry>,
    writes: usize,
    fail_at: Option<usize>,
    fail_cleanup: bool,
    concurrent_activation: bool,
}
impl Fake {
    fn new() -> Self {
        Self {
            entries: BTreeMap::from([(key(Entry::Task(10, 7)), Entry::Task(10, 7))]),
            writes: 0,
            fail_at: None,
            fail_cleanup: false,
            concurrent_activation: false,
        }
    }
}
impl Maps for Fake {
    fn task(&self, pid: i32) -> io::Result<Option<u32>> {
        Ok(self
            .entries
            .get(&(4, pid, 0, 0))
            .and_then(|entry| match entry {
                Entry::Task(_, domain) => Some(*domain),
                _ => None,
            }))
    }
    fn inherited(&self, _: i32, child: i32, domain: u32) -> io::Result<Vec<Entry>> {
        let state = ProcState {
            labels: 3,
            lin_gates: 9,
        };
        Ok(vec![
            Entry::Global(child, state),
            Entry::Domain(child, domain, state),
            Entry::Root(child, 10),
            Entry::Provenance(child, 0, 1, Origin([42; 21])),
            Entry::Provenance(child, domain, 2, Origin([99; 21])),
            Entry::Task(child, domain),
        ])
    }
    fn insert(&mut self, entry: Entry) -> io::Result<()> {
        self.writes += 1;
        if self.fail_at == Some(self.writes) {
            if self.concurrent_activation {
                self.entries.insert((4, 20, 0, 0), Entry::Task(20, 7));
            }
            return Err(io::Error::other("injected map failure"));
        }
        if self.entries.contains_key(&key(entry)) {
            return Err(io::Error::other("EEXIST"));
        }
        if matches!(entry, Entry::Task(..)) {
            assert!(
                self.entries.contains_key(&(3, 20, 7, 2)),
                "provenance must precede activation"
            );
        }
        self.entries.insert(key(entry), entry);
        Ok(())
    }
    fn matches(&self, entry: Entry) -> io::Result<bool> {
        Ok(self
            .entries
            .get(&key(entry))
            .is_some_and(|existing| match (*existing, entry) {
                (Entry::Global(_, a), Entry::Global(_, b))
                | (Entry::Domain(_, _, a), Entry::Domain(_, _, b)) => {
                    a.labels == b.labels && a.lin_gates == b.lin_gates
                }
                (Entry::Root(_, a), Entry::Root(_, b)) => a == b,
                (Entry::Provenance(_, _, _, a), Entry::Provenance(_, _, _, b)) => a == b,
                (Entry::Task(_, a), Entry::Task(_, b)) => a == b,
                _ => false,
            }))
    }
    fn remove(&mut self, entry: Entry) -> io::Result<()> {
        if self.fail_cleanup {
            return Err(io::Error::other("injected cleanup failure"));
        }
        self.entries.remove(&key(entry));
        Ok(())
    }
}

#[test]
fn every_partial_write_is_preserved_and_same_pid_can_retry() {
    for failed_stage in 1..=6 {
        let mut maps = Fake {
            fail_at: Some(failed_stage),
            ..Fake::new()
        };
        assert!(adopt_maps(&mut maps, 10, 20, 7).is_err());
        assert_eq!(
            maps.entries.len(),
            failed_stage,
            "stage {failed_stage} lost partial state"
        );
        assert_eq!(maps.task(20).unwrap(), None);
        maps.fail_at = None;
        assert!(adopt_maps(&mut maps, 10, 20, 7).unwrap());
        assert_eq!(maps.task(20).unwrap(), Some(7));
    }
}

#[test]
fn existing_collision_and_partial_entries_are_preserved() {
    let mut maps = Fake::new();
    maps.entries.insert((2, 20, 0, 0), Entry::Root(20, 999));
    assert!(adopt_maps(&mut maps, 10, 20, 7).is_err());
    assert_eq!(maps.entries.len(), 4);
    assert!(matches!(
        maps.entries.get(&(2, 20, 0, 0)),
        Some(Entry::Root(_, 999))
    ));
}

#[test]
fn failed_insert_never_attempts_destructive_cleanup() {
    let mut maps = Fake {
        fail_at: Some(2),
        fail_cleanup: true,
        ..Fake::new()
    };
    assert!(adopt_maps(&mut maps, 10, 20, 7)
        .unwrap_err()
        .to_string()
        .contains("partial state preserved"));
    let mut maps = Fake {
        fail_at: Some(3),
        concurrent_activation: true,
        ..Fake::new()
    };
    assert!(adopt_maps(&mut maps, 10, 20, 7)
        .unwrap_err()
        .to_string()
        .contains("partial state preserved"));
    assert_eq!(maps.task(20).unwrap(), Some(7));
    assert!(maps.entries.contains_key(&(1, 20, 7, 0)));
}

#[test]
fn fork_activation_after_inactive_observation_preserves_all_state() {
    use std::cell::{Cell, RefCell};
    struct LateFork {
        maps: RefCell<Fake>,
        observed_inactive: Cell<bool>,
        activated: Cell<bool>,
    }
    impl LateFork {
        fn fork(&self) {
            if self.activated.replace(true) {
                return;
            }
            let entries = self.maps.borrow().inherited(10, 20, 7).unwrap();
            for entry in entries {
                self.maps.borrow_mut().entries.insert(key(entry), entry);
            }
        }
    }
    impl Maps for LateFork {
        fn task(&self, pid: i32) -> io::Result<Option<u32>> {
            let result = self.maps.borrow().task(pid);
            if pid == 20 && result.as_ref().is_ok_and(Option::is_none) {
                self.observed_inactive.set(true);
            }
            result
        }
        fn inherited(&self, parent: i32, child: i32, domain: u32) -> io::Result<Vec<Entry>> {
            self.maps.borrow().inherited(parent, child, domain)
        }
        fn insert(&mut self, entry: Entry) -> io::Result<()> {
            self.maps.borrow_mut().insert(entry)
        }
        fn matches(&self, entry: Entry) -> io::Result<bool> {
            // 第三步插入失败之后、可能的删除之前，BPF fork 建立整组状态。
            if self.maps.borrow().writes == 3 && self.observed_inactive.get() {
                self.fork();
            }
            self.maps.borrow().matches(entry)
        }
        fn remove(&mut self, _: Entry) -> io::Result<()> {
            self.fork();
            panic!("must not delete concurrently activated state");
        }
    }
    let mut maps = LateFork {
        maps: RefCell::new(Fake {
            fail_at: Some(3),
            ..Fake::new()
        }),
        observed_inactive: Cell::new(false),
        activated: Cell::new(false),
    };
    assert!(adopt_maps(&mut maps, 10, 20, 7).unwrap());
    assert!(maps.activated.get());
    for entry in maps.inherited(10, 20, 7).unwrap() {
        assert!(maps.matches(entry).unwrap());
    }
    assert_eq!(maps.task(20).unwrap(), Some(7));
}

#[test]
fn both_global_and_domain_provenance_are_copied_without_changing_origin() {
    let mut maps = Fake::new();
    assert!(adopt_maps(&mut maps, 10, 20, 7).unwrap());
    for (domain, label, expected) in [(0, 1, 42), (7, 2, 99)] {
        match maps.entries.get(&(3, 20, domain, label)).unwrap() {
            Entry::Provenance(_, _, _, value) => assert_eq!(*value, Origin([expected; 21])),
            _ => panic!("wrong entry type"),
        }
    }
    assert!(!adopt_maps(&mut maps, 10, 20, 7).unwrap());
    assert_eq!(maps.writes, 6);
}

#[test]
#[ignore = "requires root, BPF LSM and a private ACTPLANE_BPF_PIN_ROOT; serial only"]
fn live_map_partial_retry_provenance_and_revoke_allow_same_pid_retry() {
    use std::process::Command;
    let pin = std::env::var("ACTPLANE_BPF_PIN_ROOT").expect("explicit private pin root");
    assert!(pin.starts_with("/sys/fs/bpf/issue10385-review-"));
    let engine = PinnedEngine::open_or_install_singleton().unwrap();
    let parent = std::process::id() as i32;
    let child = Command::new("sleep").arg("30").spawn().unwrap();
    struct Stop(std::process::Child);
    impl Drop for Stop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let pid = child.id() as i32;
    let _stop = Stop(child);
    let domain = 12345;
    engine.seed_label_in_domain(parent, domain, 1).unwrap();
    let mut maps = PinnedMaps::open(&engine.paths).unwrap();
    maps.proc
        .insert(
            PidDomainKey {
                pid: parent,
                domain_id: domain,
            },
            ProcState {
                labels: 2,
                lin_gates: 9,
            },
            BPF_ANY,
        )
        .unwrap();
    let global_origin = Origin([42; 21]);
    let domain_origin = Origin([99; 21]);
    maps.origins
        .insert(
            PidLabelKey {
                pid: parent,
                domain_id: 0,
                label: 1,
            },
            global_origin,
            BPF_ANY,
        )
        .unwrap();
    maps.origins
        .insert(
            PidLabelKey {
                pid: parent,
                domain_id: domain,
                label: 2,
            },
            domain_origin,
            BPF_ANY,
        )
        .unwrap();
    // 第三步发生真实 EEXIST：部分状态和冲突 key 均保留，重试只复用完全匹配的值。
    maps.roots.insert(pid, 999, BPF_NOEXIST).unwrap();
    assert!(engine.adopt_child(parent, pid, domain).is_err());
    assert!(maps.task(pid).unwrap().is_none());
    assert!(map_get_optional(&maps.global, &pid, "global")
        .unwrap()
        .is_some());
    assert!(map_get_optional(
        &maps.proc,
        &PidDomainKey {
            pid,
            domain_id: domain
        },
        "domain"
    )
    .unwrap()
    .is_some());
    assert_eq!(map_get(&maps.roots, &pid, "collision").unwrap(), 999);
    maps.roots.remove(&pid).unwrap();
    assert!(engine.adopt_child(parent, pid, domain).unwrap());
    for (domain_id, label, expected) in [(0, 1, global_origin), (domain, 2, domain_origin)] {
        let origin = map_get(
            &maps.origins,
            &PidLabelKey {
                pid,
                domain_id,
                label,
            },
            "child provenance",
        )
        .unwrap();
        assert_eq!(origin, expected);
    }
    assert_eq!(maps.task(pid).unwrap(), Some(domain));
    engine.revoke_adopted_child(pid, domain).unwrap();
    assert!(map_get_optional(&maps.global, &pid, "global")
        .unwrap()
        .is_none());
    assert!(map_get_optional(&maps.roots, &pid, "root")
        .unwrap()
        .is_none());
    assert!(map_get_optional(
        &maps.origins,
        &PidLabelKey {
            pid,
            domain_id: domain,
            label: 2
        },
        "origin"
    )
    .unwrap()
    .is_none());
    assert!(engine.adopt_child(parent, pid, domain).unwrap());
    engine.revoke_adopted_child(pid, domain).unwrap();
    engine.unbind_pid_from_domain(parent, domain).unwrap();
}
