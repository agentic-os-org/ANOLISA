//! Scope-owned process matching and selection cache.
//!
//! The daemon supplies process observations; this module owns matching and
//! deduplication. The worker submits selected instances to PAP for Binding admission.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use asc_policy_types::Validate;
#[cfg(test)]
use asc_policy_types::policy::PreparedPolicy;
use asc_policy_types::process_discovery::ProcessIdentity;
use asc_policy_types::scope::{PreparedScope, ProcessMatcher, ScopeSelector};

/// Matching inputs; executable inode changes also invalidate a cached result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessFingerprint {
    /// Kernel-provided executable link, not caller-controlled `argv[0]`.
    pub executable: PathBuf,
    /// Kernel process name; changes invalidate cached name matches.
    pub process_name: String,
    /// Executable device number.
    pub device: u64,
    /// Executable inode number.
    pub inode: u64,
}

/// One verified process snapshot supplied by the procfs adapter.
pub struct ProcessObservation {
    /// Process instance, verified before and after metadata reads.
    pub identity: ProcessIdentity,
    /// Inputs used for matching.
    pub fingerprint: ProcessFingerprint,
}

struct CheckedProcess {
    identity: ProcessIdentity,
    fingerprint: ProcessFingerprint,
    matches: bool,
}

/// Counters for one scan; unchanged processes do not incur another match.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScanResult {
    /// Executable matching operations performed this round.
    pub matched_checks: usize,
    /// Already-checked observations with unchanged inputs.
    pub cached: usize,
    /// New process instances selected this round.
    pub instances_selected: usize,
}

/// One job's state. The owning daemon worker serializes all access.
pub struct ScopeDiscoveryState {
    selector: ScopeSelector,
    pinned_pid: Option<ProcessIdentity>,
    checked: BTreeMap<u32, CheckedProcess>,
    instances: BTreeMap<u32, ProcessIdentity>,
}

impl ScopeDiscoveryState {
    /// Creates a single discovery cache shared by all policies in an assignment.
    /// # Errors
    /// Rejects invalid selectors or snapshots inconsistent with the references.
    pub fn for_scope(scope: PreparedScope) -> Result<Self, &'static str> {
        Self::from_seed(asc_pap::ScopeDiscoverySeed {
            scope,
            pinned_process: None,
            instances: Vec::new(),
        })
    }

    /// Restores known instances before scanning, including those unreadable on the first scan.
    /// # Errors
    /// Rejects invalid assignments and unsupported selectors.
    pub fn from_seed(seed: asc_pap::ScopeDiscoverySeed) -> Result<Self, &'static str> {
        let asc_pap::ScopeDiscoverySeed {
            scope,
            pinned_process,
            instances,
        } = seed;
        scope.validate().map_err(|_| "invalid scope assignment")?;
        if matches!(scope.selector, ScopeSelector::CgroupId { .. }) {
            return Err("cgroup discovery is unsupported");
        }
        Ok(Self {
            selector: scope.selector,
            pinned_pid: pinned_process,
            checked: BTreeMap::new(),
            instances: instances
                .into_iter()
                .map(|identity| (identity.pid, identity))
                .collect(),
        })
    }

    /// Applies a scan. `present` includes PIDs whose metadata could not be read.
    /// Only a complete enumeration may remove absent processes. A failed read
    /// never erases a selected instance or its deduplication key.
    pub fn reconcile(
        &mut self,
        observations: Vec<ProcessObservation>,
        present: &BTreeSet<u32>,
        complete: bool,
    ) -> ScanResult {
        let mut result = ScanResult::default();
        for observation in observations {
            let ProcessObservation {
                identity,
                fingerprint,
            } = observation;
            // A successfully observed replacement proves the previous instance exited.
            if self
                .instances
                .get(&identity.pid)
                .is_some_and(|previous| previous != &identity)
            {
                self.instances.remove(&identity.pid);
            }
            let matches = if let Some(previous) =
                self.checked.get(&identity.pid).filter(|previous| {
                    previous.identity == identity && previous.fingerprint == fingerprint
                }) {
                result.cached += 1;
                previous.matches
            } else {
                result.matched_checks += 1;
                let matches = match &self.selector {
                    ScopeSelector::Process {
                        matcher: ProcessMatcher::Name { process_name },
                    } => &fingerprint.process_name == process_name,
                    ScopeSelector::Process {
                        matcher: ProcessMatcher::Executable { executable },
                    } => fingerprint.executable.as_os_str() == std::ffi::OsStr::new(executable),
                    ScopeSelector::Pid { pid } => {
                        let selected = *pid == identity.pid
                            && self.pinned_pid.as_ref().is_none_or(|p| p == &identity);
                        if selected {
                            self.pinned_pid = Some(identity.clone());
                        }
                        selected
                    }
                    ScopeSelector::CgroupId { .. } => false,
                };
                self.checked.insert(
                    identity.pid,
                    CheckedProcess {
                        identity: identity.clone(),
                        fingerprint,
                        matches,
                    },
                );
                matches
            };
            if !matches {
                self.instances.remove(&identity.pid);
            }
            if matches && !self.instances.contains_key(&identity.pid) {
                // The worker submits the full selection every scan, so a failed PAP
                // admission is retried even when this match is cached.
                self.instances.insert(identity.pid, identity);
                result.instances_selected += 1;
            }
        }
        if complete {
            self.checked.retain(|pid, _| present.contains(pid));
            self.instances.retain(|pid, _| present.contains(pid));
        }
        result
    }

    /// Selected process instances; PAP admits their Bindings from the saved Scope.
    pub fn instances(&self) -> impl Iterator<Item = &ProcessIdentity> {
        self.instances.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> PreparedPolicy {
        let binding: serde_json::Value = serde_json::from_str(include_str!(
            "../../asc-policy-types/tests/fixtures/prepared-binding.json"
        ))
        .unwrap();
        serde_json::from_value(binding["policy"].clone()).unwrap()
    }

    fn state() -> ScopeDiscoveryState {
        ScopeDiscoveryState::for_scope(scope()).unwrap()
    }

    fn scope() -> PreparedScope {
        use asc_policy_types::identifiers::ResourceId;
        PreparedScope {
            scope_id: ResourceId::new("scope-one").unwrap(),
            status: asc_policy_types::scope::ScopeStatus::Active,
            selector: ScopeSelector::Process {
                matcher: ProcessMatcher::Executable {
                    executable: "/bin/agent".to_owned(),
                },
            },
            policy_snapshots: vec![policy()],
        }
    }

    fn process(pid: u32, start_time: u64, executable: &str) -> ProcessObservation {
        ProcessObservation {
            identity: ProcessIdentity {
                boot_id: "boot-one".to_owned(),
                pid_namespace: "pid:[42]".to_owned(),
                pid,
                start_time,
            },
            fingerprint: ProcessFingerprint {
                executable: executable.into(),
                process_name: "agent".to_owned(),
                device: 1,
                inode: 2,
            },
        }
    }

    #[test]
    fn restart_seed_preserves_unreadable_instance_and_pid_pin() {
        let mut scope = scope();
        scope.selector = ScopeSelector::Pid { pid: 10 };
        let identity = process(10, 1, "/bin/agent").identity;
        let mut recovered = ScopeDiscoveryState::from_seed(asc_pap::ScopeDiscoverySeed {
            scope,
            pinned_process: Some(identity.clone()),
            instances: vec![identity],
        })
        .unwrap();
        recovered.reconcile(Vec::new(), &BTreeSet::new(), false);
        assert_eq!(recovered.instances().count(), 1);
        recovered.reconcile(Vec::new(), &BTreeSet::from([10]), true);
        assert_eq!(recovered.instances().count(), 1);
        recovered.reconcile(Vec::new(), &BTreeSet::new(), true);
        assert_eq!(recovered.instances().count(), 0);
        recovered.reconcile(
            vec![process(10, 2, "/bin/agent")],
            &BTreeSet::from([10]),
            true,
        );
        assert_eq!(recovered.instances().count(), 0);
    }

    #[test]
    fn pid_selector_pins_first_instance_and_does_not_follow_reuse() {
        let mut scope = scope();
        scope.selector = ScopeSelector::Pid { pid: 10 };
        let mut state = ScopeDiscoveryState::for_scope(scope).unwrap();
        let present = BTreeSet::from([10]);
        state.reconcile(vec![process(10, 1, "/bin/agent")], &present, true);
        assert_eq!(state.instances().count(), 1);
        state.reconcile(Vec::new(), &BTreeSet::new(), true);
        assert_eq!(state.instances().count(), 0);
        state.reconcile(vec![process(10, 2, "/bin/agent")], &present, true);
        assert_eq!(state.instances().count(), 0);
    }

    #[test]
    fn scope_name_matching_rechecks_rename_exec_and_pid_reuse() {
        let policy = policy();
        let scope = PreparedScope {
            scope_id: policy.policy_id.clone(),
            status: asc_policy_types::scope::ScopeStatus::Active,
            selector: ScopeSelector::Process {
                matcher: ProcessMatcher::Name {
                    process_name: "agent".to_owned(),
                },
            },
            policy_snapshots: vec![policy],
        };
        let mut state = ScopeDiscoveryState::for_scope(scope).unwrap();
        let present = BTreeSet::from([10]);
        assert_eq!(
            state
                .reconcile(vec![process(10, 1, "/bin/node")], &present, true)
                .instances_selected,
            1
        );
        assert_eq!(
            state
                .reconcile(vec![process(10, 1, "/bin/node")], &present, true)
                .cached,
            1
        );
        let mut renamed = process(10, 1, "/bin/node");
        renamed.fingerprint.process_name = "other".to_owned();
        assert_eq!(
            state
                .reconcile(vec![renamed], &present, true)
                .matched_checks,
            1
        );
        assert_eq!(state.instances().count(), 0);
        assert_eq!(
            state
                .reconcile(vec![process(10, 2, "/bin/node")], &present, true)
                .instances_selected,
            1
        );
        assert_eq!(state.instances().next().unwrap().start_time, 2);
    }

    #[test]
    fn djob_scope_caches_matches_and_nonmatches_but_rechecks_exec_and_pid_reuse() {
        let mut state = state();
        let present = BTreeSet::from([10, 20]);
        let first = state.reconcile(
            vec![process(10, 1, "/bin/agent"), process(20, 2, "/bin/shell")],
            &present,
            true,
        );
        assert_eq!(
            first,
            ScanResult {
                matched_checks: 2,
                cached: 0,
                instances_selected: 1
            }
        );
        let repeated = state.reconcile(
            vec![process(10, 1, "/bin/agent"), process(20, 2, "/bin/shell")],
            &present,
            true,
        );
        assert_eq!(
            repeated,
            ScanResult {
                matched_checks: 0,
                cached: 2,
                instances_selected: 0
            }
        );
        let exec = state.reconcile(vec![process(20, 2, "/bin/agent")], &present, true);
        assert_eq!(exec.instances_selected, 1);
        assert_eq!(state.instances().count(), 2);
        let reused = state.reconcile(vec![process(10, 3, "/bin/agent")], &present, true);
        assert_eq!(reused.instances_selected, 1);
        assert_eq!(state.instances().count(), 2);
        assert_eq!(state.instances.get(&10).unwrap().start_time, 3);
        let mut replaced = process(10, 3, "/bin/agent");
        replaced.fingerprint.inode = 99;
        let replacement = state.reconcile(vec![replaced], &present, true);
        assert_eq!(replacement.matched_checks, 1);
        assert_eq!(replacement.instances_selected, 0);
    }

    #[test]
    fn djob_scope_preserves_unreadable_instances_and_prunes_only_complete_scans() {
        let mut state = state();
        state.reconcile(
            vec![process(10, 1, "/bin/agent")],
            &BTreeSet::from([10]),
            true,
        );
        state.reconcile(Vec::new(), &BTreeSet::new(), false);
        assert_eq!(state.instances().count(), 1);
        state.reconcile(Vec::new(), &BTreeSet::from([10]), true);
        assert_eq!(state.instances().count(), 1);
        let recovered = state.reconcile(
            vec![process(10, 1, "/bin/agent")],
            &BTreeSet::from([10]),
            true,
        );
        assert_eq!(recovered.instances_selected, 0);
        assert_eq!(recovered.cached, 1);
        state.reconcile(Vec::new(), &BTreeSet::new(), true);
        assert_eq!(state.instances().count(), 0);
        assert!(state.checked.is_empty());
    }

    #[test]
    fn selection_is_per_scope_and_independent_of_policy_count() {
        let mut first = state();
        let mut scope = scope();
        scope.scope_id = asc_foundation_types::ResourceId::new("scope-two").unwrap();
        let mut policy = policy();
        policy.policy_id = asc_foundation_types::ResourceId::new("policy-two").unwrap();
        scope.policy_snapshots.push(policy);
        let mut second = ScopeDiscoveryState::for_scope(scope).unwrap();
        for state in [&mut first, &mut second] {
            let selected = state.reconcile(
                vec![process(10, 1, "/bin/agent"), process(10, 1, "/bin/agent")],
                &BTreeSet::from([10]),
                true,
            );
            assert_eq!(selected.instances_selected, 1);
            assert_eq!(
                state.instances().cloned().collect::<Vec<_>>(),
                vec![process(10, 1, "/bin/agent").identity]
            );
        }
        first.reconcile(Vec::new(), &BTreeSet::new(), true);
        assert_eq!(first.instances().count(), 0);
        assert_eq!(second.instances().count(), 1);
    }
}
