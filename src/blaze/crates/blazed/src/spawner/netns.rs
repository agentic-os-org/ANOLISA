// SPDX-License-Identifier: Apache-2.0
//! Per-VM network namespace allocation and compensated setup.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use blaze_core::{BlazeError, Result};
use thiserror::Error;
use tokio::process::Command;
use uuid::Uuid;

use super::HOST_NETWORK_COORDINATION_PATH;

const NET_VETH_BASE: usize = 4;
const NET_VETH_TOP: usize = 0x1_0000;
const NET_MAX_SLOT: usize = (NET_VETH_TOP - NET_VETH_BASE) / 4;
const HOST_LOCK_RETRY: Duration = Duration::from_millis(10);

#[derive(Debug, Default)]
struct SlotState {
    used: HashSet<usize>,
    next: usize,
}

#[derive(Debug, Clone)]
pub(super) struct IpOutput {
    pub(super) success: bool,
    pub(super) status: String,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
}

#[async_trait]
pub(super) trait IpCommandRunner: Send + Sync {
    async fn output(&self, args: &[String], timeout: Duration) -> Result<IpOutput>;

    #[cfg(target_os = "linux")]
    fn executable_in_path(&self, _name: &str) -> bool {
        true
    }

    #[cfg(target_os = "linux")]
    fn has_network_admin(&self) -> bool {
        true
    }
}

struct SystemIpCommandRunner;

#[async_trait]
impl IpCommandRunner for SystemIpCommandRunner {
    async fn output(&self, args: &[String], timeout: Duration) -> Result<IpOutput> {
        let mut command = Command::new("ip");
        command.kill_on_drop(true).env("LC_ALL", "C").args(args);
        let output = tokio::time::timeout(timeout, command.output())
            .await
            .map_err(|_| BlazeError::BackendError {
                msg: format!("ip {} timed out", args.join(" ")),
            })??;
        Ok(IpOutput {
            success: output.status.success(),
            status: output.status.to_string(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    #[cfg(target_os = "linux")]
    fn executable_in_path(&self, name: &str) -> bool {
        executable_in_path(name)
    }

    #[cfg(target_os = "linux")]
    fn has_network_admin(&self) -> bool {
        // The current implementation relies on root-owned netns mounts,
        // tap creation, forwarding changes, and NAT rules.
        unsafe { libc::geteuid() == 0 }
    }
}

/// Process-local allocator and lifecycle owner for Blaze network namespaces.
pub(super) struct NetworkManager {
    state: Mutex<SlotState>,
    command_timeout: Duration,
    runner: Arc<dyn IpCommandRunner>,
    coordination_file: Option<PathBuf>,
}

impl fmt::Debug for NetworkManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NetworkManager")
            .field("command_timeout", &self.command_timeout)
            .finish_non_exhaustive()
    }
}

impl Default for NetworkManager {
    fn default() -> Self {
        Self {
            state: Mutex::new(SlotState::default()),
            command_timeout: Duration::from_secs(5),
            runner: Arc::new(SystemIpCommandRunner),
            coordination_file: Some(PathBuf::from(HOST_NETWORK_COORDINATION_PATH)),
        }
    }
}

struct HostNetworkGuard {
    #[cfg(unix)]
    _file: Option<std::fs::File>,
}

/// One fully configured per-VM network namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NetworkSlot {
    slot: usize,
    owner: Uuid,
    netns: String,
    tap_name: String,
    veth_host: String,
    veth_peer: String,
}

/// Network setup failure with an optional residual slot owner.
#[derive(Debug, Error)]
#[error("{source}")]
pub(super) struct NetworkCreateError {
    #[source]
    source: BlazeError,
    residual: Option<NetworkSlot>,
}

impl NetworkCreateError {
    fn clean(source: BlazeError) -> Self {
        Self {
            source,
            residual: None,
        }
    }

    fn with_residual(source: BlazeError, residual: NetworkSlot) -> Self {
        Self {
            source,
            residual: Some(residual),
        }
    }

    /// Split the setup error from any network slot that still needs cleanup.
    pub(super) fn into_parts(self) -> (BlazeError, Option<NetworkSlot>) {
        (self.source, self.residual)
    }
}

impl From<BlazeError> for NetworkCreateError {
    fn from(source: BlazeError) -> Self {
        Self::clean(source)
    }
}

impl NetworkSlot {
    pub(super) fn from_record(slot: usize, owner: Uuid) -> Result<Self> {
        if slot >= NET_MAX_SLOT {
            return Err(BlazeError::BackendError {
                msg: format!("network slot {slot} is outside 0..{NET_MAX_SLOT}"),
            });
        }
        Ok(network_slot(slot, owner))
    }

    pub(super) fn slot(&self) -> usize {
        self.slot
    }

    pub(super) fn owner(&self) -> Uuid {
        self.owner
    }

    /// Network namespace name.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(super) fn netns(&self) -> &str {
        &self.netns
    }

    /// Tap device visible inside the namespace.
    pub(super) fn tap_name(&self) -> &str {
        &self.tap_name
    }
}

impl NetworkManager {
    #[cfg(test)]
    pub(super) fn with_runner(runner: Arc<dyn IpCommandRunner>) -> Self {
        Self {
            state: Mutex::new(SlotState::default()),
            command_timeout: Duration::from_secs(1),
            runner,
            coordination_file: None,
        }
    }

    #[cfg(test)]
    pub(super) fn with_runner_and_lock(
        runner: Arc<dyn IpCommandRunner>,
        coordination_file: PathBuf,
    ) -> Self {
        Self {
            state: Mutex::new(SlotState::default()),
            command_timeout: Duration::from_secs(1),
            runner,
            coordination_file: Some(coordination_file),
        }
    }

    /// Check the commands and host conditions required by the network path.
    pub(super) async fn probe(&self) -> Result<bool> {
        #[cfg(not(target_os = "linux"))]
        {
            Ok(false)
        }
        #[cfg(target_os = "linux")]
        {
            if !self.runner.has_network_admin()
                || ["ip", "sysctl", "iptables"]
                    .iter()
                    .any(|command| !self.runner.executable_in_path(command))
            {
                return Ok(false);
            }
            let args = vec!["netns".to_string(), "list".to_string()];
            Ok(self.run_ip_output(&args).await?.success)
        }
    }

    /// Create an isolated namespace, veth uplink, tap, route, and NAT rule.
    ///
    /// `record` runs immediately after this manager creates the namespace and
    /// before any dependent resources are added. Callers use it to publish
    /// ownership without ever recording a namespace created by another process.
    pub(super) async fn create<F>(
        &self,
        owner: Uuid,
        mut record: F,
    ) -> std::result::Result<NetworkSlot, NetworkCreateError>
    where
        F: FnMut(&NetworkSlot) -> Result<()>,
    {
        let _host_guard = self.acquire_host_guard().await?;
        let mut blocked = self.existing_slots().await?;
        let (slot, network) = loop {
            let slot = self.allocate(&blocked)?;
            let network = network_slot(slot, owner);
            let add_namespace = vec!["netns".into(), "add".into(), network.netns.clone()];
            match self.run_ip(&add_namespace).await {
                Ok(()) => {
                    if let Err(error) = record(&network) {
                        return self.fail_setup(&network, error).await;
                    }
                    break (slot, network);
                }
                Err(error) => {
                    let refreshed = match self.list_namespaces().await {
                        Ok(output) => output,
                        Err(confirmation) => {
                            let error = BlazeError::BackendError {
                                msg: format!(
                                    "{error}; cannot determine whether namespace {} was created: {confirmation}",
                                    network.netns
                                ),
                            };
                            let error = match record(&network) {
                                Ok(()) => error,
                                Err(recording) => BlazeError::BackendError {
                                    msg: format!(
                                        "{error}; cannot record uncertain namespace ownership: {recording}"
                                    ),
                                },
                            };
                            return self.fail_setup(&network, error).await;
                        }
                    };
                    if namespace_exists_in_output(&refreshed.stdout, &network.netns) {
                        let error = match record(&network) {
                            Ok(()) => error,
                            Err(recording) => BlazeError::BackendError {
                                msg: format!(
                                    "{error}; cannot record created namespace ownership: {recording}"
                                ),
                            },
                        };
                        return self.fail_setup(&network, error).await;
                    }
                    self.release(slot);
                    let refreshed = parse_existing_slots(&refreshed.stdout);
                    if refreshed.contains(&slot) {
                        blocked.extend(refreshed);
                        continue;
                    }
                    return Err(NetworkCreateError::clean(error));
                }
            }
        };
        let (host_ip, peer_ip) = veth_ips(slot);
        let add_veth = vec![
            "link".into(),
            "add".into(),
            network.veth_host.clone(),
            "type".into(),
            "veth".into(),
            "peer".into(),
            "name".into(),
            network.veth_peer.clone(),
            "netns".into(),
            network.netns.clone(),
        ];
        if let Err(error) = self.run_ip(&add_veth).await {
            return self.fail_setup(&network, error).await;
        }
        let host_steps = vec![
            vec![
                "addr".into(),
                "add".into(),
                format!("{host_ip}/30"),
                "dev".into(),
                network.veth_host.clone(),
            ],
            vec![
                "link".into(),
                "set".into(),
                network.veth_host.clone(),
                "up".into(),
            ],
        ];
        for args in host_steps {
            if let Err(error) = self.run_ip(&args).await {
                return self.fail_setup(&network, error).await;
            }
        }

        let ns_steps = vec![
            vec![
                "ip".into(),
                "addr".into(),
                "add".into(),
                format!("{peer_ip}/30"),
                "dev".into(),
                network.veth_peer.clone(),
            ],
            vec![
                "ip".into(),
                "link".into(),
                "set".into(),
                network.veth_peer.clone(),
                "up".into(),
            ],
            vec![
                "ip".into(),
                "link".into(),
                "set".into(),
                "lo".into(),
                "up".into(),
            ],
            vec![
                "ip".into(),
                "tuntap".into(),
                "add".into(),
                network.tap_name.clone(),
                "mode".into(),
                "tap".into(),
            ],
            vec![
                "ip".into(),
                "addr".into(),
                "add".into(),
                "169.254.0.1/30".into(),
                "dev".into(),
                network.tap_name.clone(),
            ],
            vec![
                "ip".into(),
                "link".into(),
                "set".into(),
                network.tap_name.clone(),
                "up".into(),
            ],
            vec![
                "ip".into(),
                "route".into(),
                "add".into(),
                "default".into(),
                "via".into(),
                host_ip,
            ],
            vec!["sysctl".into(), "-w".into(), "net.ipv4.ip_forward=1".into()],
            vec![
                "iptables".into(),
                "-t".into(),
                "nat".into(),
                "-A".into(),
                "POSTROUTING".into(),
                "-s".into(),
                "169.254.0.2".into(),
                "-o".into(),
                network.veth_peer.clone(),
                "-j".into(),
                "SNAT".into(),
                "--to".into(),
                peer_ip,
            ],
        ];
        for command in ns_steps {
            if let Err(error) = self.run_in_namespace(&network.netns, &command).await {
                return self.fail_setup(&network, error).await;
            }
        }
        Ok(network)
    }

    /// Remove all resources for a slot and return it to the allocator.
    pub(super) async fn destroy(&self, network: &NetworkSlot) -> Result<()> {
        let _host_guard = self.acquire_host_guard().await?;
        self.cleanup_commands(network).await?;
        self.release(network.slot);
        Ok(())
    }

    async fn acquire_host_guard(&self) -> Result<HostNetworkGuard> {
        let Some(path) = self.coordination_file.as_deref() else {
            return Ok(HostNetworkGuard {
                #[cfg(unix)]
                _file: None,
            });
        };
        acquire_host_guard(path, self.command_timeout).await
    }

    fn allocate(&self, blocked: &HashSet<usize>) -> Result<usize> {
        let mut state = self.state.lock().map_err(|_| BlazeError::BackendError {
            msg: "network slot allocator lock poisoned".to_string(),
        })?;
        for offset in 0..NET_MAX_SLOT {
            let slot = (state.next + offset) % NET_MAX_SLOT;
            if !blocked.contains(&slot) && state.used.insert(slot) {
                state.next = (slot + 1) % NET_MAX_SLOT;
                return Ok(slot);
            }
        }
        Err(BlazeError::BackendError {
            msg: format!("network slots exhausted (max {NET_MAX_SLOT})"),
        })
    }

    async fn existing_slots(&self) -> Result<HashSet<usize>> {
        let output = self.list_namespaces().await?;
        Ok(parse_existing_slots(&output.stdout))
    }

    async fn list_namespaces(&self) -> Result<IpOutput> {
        let args = vec!["netns".to_string(), "list".to_string()];
        let output = self.run_ip_output(&args).await?;
        if !output.success {
            return Err(command_error(&args, &output, "listing namespaces"));
        }
        Ok(output)
    }

    fn release(&self, slot: usize) {
        if let Ok(mut state) = self.state.lock() {
            state.used.remove(&slot);
        }
    }

    async fn cleanup_commands(&self, network: &NetworkSlot) -> Result<()> {
        if !self.namespace_exists(&network.netns).await? {
            return Ok(());
        }
        self.run_in_namespace_cleanup(
            &network.netns,
            &[
                "ip".into(),
                "link".into(),
                "del".into(),
                network.veth_peer.clone(),
            ],
        )
        .await?;
        self.delete_namespace(&network.netns).await?;
        Ok(())
    }

    pub(super) async fn find_by_owner(&self, owner: Uuid) -> Result<Option<NetworkSlot>> {
        let output = self.list_namespaces().await?;
        Ok(parse_existing_networks(&output.stdout)
            .into_iter()
            .find(|network| network.owner == owner))
    }

    async fn namespace_exists(&self, name: &str) -> Result<bool> {
        let output = self.list_namespaces().await?;
        Ok(namespace_exists_in_output(&output.stdout, name))
    }

    async fn delete_namespace(&self, name: &str) -> Result<()> {
        let args = vec!["netns".to_string(), "del".to_string(), name.to_string()];
        let output = self.run_ip_output(&args).await?;
        if output.success {
            return Ok(());
        }
        let deletion = command_error(&args, &output, "cleaning up");
        match self.namespace_exists(name).await {
            Ok(false) => Ok(()),
            Ok(true) => Err(deletion),
            Err(confirmation) => Err(BlazeError::BackendError {
                msg: format!(
                    "{deletion}; cannot confirm namespace {name} was removed: {confirmation}"
                ),
            }),
        }
    }

    async fn fail_setup<T>(
        &self,
        network: &NetworkSlot,
        original: BlazeError,
    ) -> std::result::Result<T, NetworkCreateError> {
        match self.cleanup_commands(network).await {
            Ok(()) => {
                self.release(network.slot);
                Err(NetworkCreateError::clean(original))
            }
            Err(cleanup) => Err(NetworkCreateError::with_residual(
                BlazeError::BackendError {
                    msg: format!(
                        "network setup failed ({original}); cleanup failed ({cleanup}); slot {} retained",
                        network.slot
                    ),
                },
                network.clone(),
            )),
        }
    }

    async fn run_in_namespace(&self, netns: &str, command: &[String]) -> Result<()> {
        let mut args = vec!["netns".to_string(), "exec".to_string(), netns.to_string()];
        args.extend_from_slice(command);
        self.run_ip(&args).await
    }

    async fn run_in_namespace_cleanup(&self, netns: &str, command: &[String]) -> Result<()> {
        let mut args = vec!["netns".to_string(), "exec".to_string(), netns.to_string()];
        args.extend_from_slice(command);
        self.run_ip_cleanup(&args).await
    }

    async fn run_ip(&self, args: &[String]) -> Result<()> {
        let output = self.run_ip_output(args).await?;
        if output.success {
            return Ok(());
        }
        Err(command_error(args, &output, "running command"))
    }

    async fn run_ip_cleanup(&self, args: &[String]) -> Result<()> {
        let output = self.run_ip_output(args).await?;
        if output.success {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if [
            "Cannot find device",
            "No such file",
            "Invalid \"netns\" value",
        ]
        .iter()
        .any(|marker| stderr.contains(marker))
        {
            return Ok(());
        }
        Err(command_error(args, &output, "cleaning up"))
    }

    async fn run_ip_output(&self, args: &[String]) -> Result<IpOutput> {
        self.runner.output(args, self.command_timeout).await
    }
}

fn command_error(args: &[String], output: &IpOutput, context: &str) -> BlazeError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.chars().take(4096).collect::<String>();
    BlazeError::BackendError {
        msg: format!(
            "ip {} failed while {context} ({}): {}",
            args.join(" "),
            output.status,
            stderr.trim()
        ),
    }
}

#[cfg(unix)]
async fn acquire_host_guard(path: &Path, timeout: Duration) -> Result<HostNetworkGuard> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let deadline = Instant::now() + timeout;
    loop {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(HostNetworkGuard { _file: Some(file) });
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EAGAIN)
            && error.raw_os_error() != Some(libc::EWOULDBLOCK)
        {
            return Err(error.into());
        }
        if Instant::now() >= deadline {
            return Err(BlazeError::BackendError {
                msg: format!(
                    "timed out acquiring host network allocation lock {}",
                    path.display()
                ),
            });
        }
        tokio::time::sleep(HOST_LOCK_RETRY).await;
    }
}

#[cfg(not(unix))]
async fn acquire_host_guard(_path: &Path, _timeout: Duration) -> Result<HostNetworkGuard> {
    Ok(HostNetworkGuard {})
}

#[cfg(target_os = "linux")]
fn executable_in_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|directory| {
        let candidate = directory.join(name);
        if !candidate.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(candidate)
                .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        }
        #[cfg(not(unix))]
        {
            true
        }
    })
}

fn parse_existing_slots(stdout: &[u8]) -> HashSet<usize> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter_map(|name| name.strip_prefix("blz-ns-"))
        .filter_map(|suffix| suffix.split('-').next())
        .filter_map(|slot| slot.parse::<usize>().ok())
        .filter(|slot| *slot < NET_MAX_SLOT)
        .collect()
}

fn namespace_exists_in_output(stdout: &[u8], name: &str) -> bool {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .any(|candidate| candidate == name)
}

fn parse_existing_networks(stdout: &[u8]) -> Vec<NetworkSlot> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter_map(|name| name.strip_prefix("blz-ns-"))
        .filter_map(|suffix| suffix.split_once('-'))
        .filter_map(|(slot, owner)| {
            Some(network_slot(
                slot.parse::<usize>().ok()?,
                owner.parse::<Uuid>().ok()?,
            ))
        })
        .filter(|network| network.slot < NET_MAX_SLOT)
        .collect()
}

fn network_slot(slot: usize, owner: Uuid) -> NetworkSlot {
    NetworkSlot {
        slot,
        owner,
        netns: format!("blz-ns-{slot}-{owner}"),
        tap_name: "tap0".to_string(),
        veth_host: format!("blz-veth-{slot}"),
        veth_peer: format!("blz-vpeer-{slot}"),
    }
}

#[cfg(test)]
pub(super) fn test_network_slot(slot: usize) -> NetworkSlot {
    network_slot(slot, Uuid::from_u128(1))
}

fn veth_ips(slot: usize) -> (String, String) {
    let base = NET_VETH_BASE + slot * 4;
    let third = (base >> 8) & 0xff;
    (
        format!("169.254.{third}.{}", (base & 0xff) + 1),
        format!("169.254.{third}.{}", (base & 0xff) + 2),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    #[test]
    fn allocator_is_unique_and_recycles() {
        let manager = NetworkManager::default();
        let first = manager.allocate(&HashSet::new()).expect("first");
        let second = manager.allocate(&HashSet::new()).expect("second");
        assert_ne!(first, second);
        manager.release(first);
        for _ in 0..NET_MAX_SLOT {
            if manager.allocate(&HashSet::new()).expect("slot") == first {
                return;
            }
        }
        panic!("released slot was not recycled");
    }

    #[test]
    fn addresses_follow_the_slot_layout() {
        assert_eq!(
            veth_ips(0),
            ("169.254.0.5".to_string(), "169.254.0.6".to_string())
        );
        assert_eq!(
            veth_ips(63),
            ("169.254.1.1".to_string(), "169.254.1.2".to_string())
        );
    }

    #[test]
    fn existing_slot_parser_ignores_unrelated_and_invalid_names() {
        let slots = parse_existing_slots(
            b"blz-ns-0-00000000-0000-0000-0000-000000000001\n\
              blz-ns-17 (id: 2)\nunrelated\nblz-ns-nope\nblz-ns-16383\n",
        );
        assert_eq!(slots, HashSet::from([0, 17]));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn host_lock_serializes_independent_network_managers() {
        let temp = tempfile::tempdir().expect("temp");
        let lock = temp.path().join("network.lock");
        let first =
            NetworkManager::with_runner_and_lock(Arc::new(FakeIpRunner::default()), lock.clone());
        let second = NetworkManager::with_runner_and_lock(Arc::new(FakeIpRunner::default()), lock);

        let first_guard = first.acquire_host_guard().await.expect("first lock");
        let blocked =
            tokio::time::timeout(Duration::from_millis(50), second.acquire_host_guard()).await;
        assert!(
            blocked.is_err(),
            "a second daemon must not allocate while the host lock is held"
        );

        drop(first_guard);
        second
            .acquire_host_guard()
            .await
            .expect("lock becomes available");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn network_probe_checks_tools_privileges_and_ip_access() {
        let missing_tool = Arc::new(FakeIpRunner::without_tool("iptables"));
        let manager = NetworkManager::with_runner(missing_tool.clone());
        assert!(!manager.probe().await.expect("missing tool probe"));
        assert!(missing_tool.calls().is_empty());

        let unprivileged = Arc::new(FakeIpRunner::without_admin());
        let manager = NetworkManager::with_runner(unprivileged.clone());
        assert!(!manager.probe().await.expect("privilege probe"));
        assert!(unprivileged.calls().is_empty());

        let available = Arc::new(FakeIpRunner::with_responses([success(b"")]));
        let manager = NetworkManager::with_runner(available.clone());
        assert!(manager.probe().await.expect("ready probe"));
        assert_eq!(
            available.calls(),
            vec![vec!["netns".to_string(), "list".to_string()]]
        );

        let inaccessible = Arc::new(FakeIpRunner::with_responses([failure(
            "network namespace access denied",
        )]));
        let manager = NetworkManager::with_runner(inaccessible);
        assert!(!manager.probe().await.expect("inaccessible probe"));
    }

    #[tokio::test]
    async fn create_skips_namespaces_owned_by_another_process() {
        let runner = Arc::new(FakeIpRunner::with_responses([success(
            b"blz-ns-0 (id: 4)\n",
        )]));
        let manager = NetworkManager::with_runner(runner.clone());

        let owner = Uuid::from_u128(1);
        let slot = manager
            .create(owner, |_| Ok(()))
            .await
            .expect("create slot");

        assert_eq!(slot.slot, 1);
        let calls = runner.calls();
        assert!(
            calls
                .iter()
                .any(|args| args == &["netns", "add", test_network_slot(1).netns()])
        );
        assert!(
            !calls
                .iter()
                .any(|args| args == &["netns", "del", "blz-ns-0"])
        );
    }

    #[tokio::test]
    async fn create_retries_when_namespace_appears_during_allocation() {
        let runner = Arc::new(FakeIpRunner::with_responses([
            success(b""),
            failure("namespace already exists"),
            success(b"blz-ns-0\n"),
        ]));
        let manager = NetworkManager::with_runner(runner.clone());

        let owner = Uuid::from_u128(1);
        let slot = manager
            .create(owner, |_| Ok(()))
            .await
            .expect("create slot");

        assert_eq!(slot.slot, 1);
        assert!(
            runner
                .calls()
                .iter()
                .any(|args| args == &["netns", "add", test_network_slot(1).netns()])
        );
    }

    #[tokio::test]
    async fn uncertain_namespace_add_cleans_the_exact_owner() {
        let network = test_network_slot(0);
        let namespace = format!("{}\n", network.netns());
        let runner = Arc::new(FakeIpRunner::with_responses([
            success(b""),
            failure("namespace add timed out"),
            success(namespace.as_bytes()),
            success(namespace.as_bytes()),
            failure("Cannot find device blz-vpeer-0"),
            success(b""),
        ]));
        let manager = NetworkManager::with_runner(runner.clone());
        let mut recorded = None;

        let failure = manager
            .create(network.owner(), |slot| {
                recorded = Some(slot.clone());
                Ok(())
            })
            .await
            .expect_err("an uncertain add result must fail after compensation");
        let (source, residual) = failure.into_parts();

        assert!(source.to_string().contains("namespace add timed out"));
        assert_eq!(recorded, Some(network.clone()));
        assert!(residual.is_none());
        assert!(
            runner
                .calls()
                .iter()
                .any(|args| args == &["netns", "del", network.netns()])
        );
        assert!(
            !manager
                .state
                .lock()
                .expect("state lock")
                .used
                .contains(&network.slot)
        );
    }

    #[tokio::test]
    async fn uncertain_namespace_add_retains_owner_when_cleanup_fails() {
        let network = test_network_slot(0);
        let namespace = format!("{}\n", network.netns());
        let runner = Arc::new(FakeIpRunner::with_responses([
            success(b""),
            failure("namespace add timed out"),
            success(namespace.as_bytes()),
            success(namespace.as_bytes()),
            failure("Cannot find device blz-vpeer-0"),
            failure("namespace delete failed"),
            success(namespace.as_bytes()),
        ]));
        let manager = NetworkManager::with_runner(runner);
        let mut recorded = None;

        let failure = manager
            .create(network.owner(), |slot| {
                recorded = Some(slot.clone());
                Ok(())
            })
            .await
            .expect_err("failed compensation must retain ownership");
        let (source, residual) = failure.into_parts();

        assert!(source.to_string().contains("slot 0 retained"));
        assert_eq!(recorded, Some(network.clone()));
        assert_eq!(residual, Some(network.clone()));
        assert!(
            manager
                .state
                .lock()
                .expect("state lock")
                .used
                .contains(&network.slot)
        );
    }

    #[tokio::test]
    async fn failed_veth_creation_only_removes_new_namespace() {
        let runner = Arc::new(FakeIpRunner::with_responses([
            success(b""),
            success(b""),
            failure("veth already exists"),
            success(b"blz-ns-0-00000000-0000-0000-0000-000000000001\n"),
        ]));
        let manager = NetworkManager::with_runner(runner.clone());

        manager
            .create(Uuid::from_u128(1), |_| Ok(()))
            .await
            .expect_err("veth creation must fail");

        let calls = runner.calls();
        assert!(
            calls
                .iter()
                .any(|args| args == &["netns", "del", test_network_slot(0).netns()])
        );
        assert!(
            !calls
                .iter()
                .any(|args| args == &["link", "del", "blz-veth-0"])
        );
    }

    #[tokio::test]
    async fn failed_cleanup_returns_the_residual_slot_owner() {
        let runner = Arc::new(FakeIpRunner::with_responses([
            success(b""),
            success(b""),
            success(b""),
            failure("host address failed"),
            success(b"blz-ns-0-00000000-0000-0000-0000-000000000001\n"),
            failure("delete peer failed"),
            success(b"blz-ns-0-00000000-0000-0000-0000-000000000001\n"),
            success(b""),
            success(b""),
        ]));
        let manager = NetworkManager::with_runner(runner.clone());

        let failure = manager
            .create(Uuid::from_u128(1), |_| Ok(()))
            .await
            .expect_err("setup must fail");
        let (source, residual) = failure.into_parts();
        let residual = residual.expect("cleanup failure must retain the slot");

        assert!(source.to_string().contains("slot 0 retained"));
        assert_eq!(residual, test_network_slot(0));
        manager
            .destroy(&residual)
            .await
            .expect("a later cleanup can release the retained slot");
        let calls = runner.calls();
        assert!(calls.iter().any(|args| {
            args == &[
                "netns",
                "exec",
                test_network_slot(0).netns(),
                "ip",
                "link",
                "del",
                "blz-vpeer-0",
            ]
        }));
        assert!(
            calls
                .iter()
                .any(|args| args == &["netns", "del", test_network_slot(0).netns()])
        );
    }

    #[tokio::test]
    async fn namespace_delete_failure_retains_a_present_slot() {
        let network = test_network_slot(0);
        let namespace = format!("{}\n", network.netns());
        let runner = Arc::new(FakeIpRunner::with_responses([
            success(namespace.as_bytes()),
            success(b""),
            failure("Cannot remove namespace file: Permission denied"),
            success(namespace.as_bytes()),
        ]));
        let manager = NetworkManager::with_runner(runner);
        manager
            .state
            .lock()
            .expect("state lock")
            .used
            .insert(network.slot);

        let error = manager
            .destroy(&network)
            .await
            .expect_err("present namespace must retain its slot");

        assert!(error.to_string().contains("Permission denied"));
        assert!(
            manager
                .state
                .lock()
                .expect("state lock")
                .used
                .contains(&network.slot)
        );
    }

    #[tokio::test]
    async fn namespace_delete_failure_accepts_confirmed_absence() {
        let network = test_network_slot(0);
        let namespace = format!("{}\n", network.netns());
        let runner = Arc::new(FakeIpRunner::with_responses([
            success(namespace.as_bytes()),
            success(b""),
            failure("Cannot remove namespace file: already removed"),
            success(b""),
        ]));
        let manager = NetworkManager::with_runner(runner);
        manager
            .state
            .lock()
            .expect("state lock")
            .used
            .insert(network.slot);

        manager
            .destroy(&network)
            .await
            .expect("confirmed absence completes cleanup");

        assert!(
            !manager
                .state
                .lock()
                .expect("state lock")
                .used
                .contains(&network.slot)
        );
    }

    struct FakeIpRunner {
        responses: Mutex<VecDeque<IpOutput>>,
        calls: Mutex<Vec<Vec<String>>>,
        #[cfg(target_os = "linux")]
        unavailable_tool: Option<String>,
        #[cfg(target_os = "linux")]
        network_admin: bool,
    }

    impl Default for FakeIpRunner {
        fn default() -> Self {
            Self {
                responses: Mutex::new(VecDeque::new()),
                calls: Mutex::new(Vec::new()),
                #[cfg(target_os = "linux")]
                unavailable_tool: None,
                #[cfg(target_os = "linux")]
                network_admin: true,
            }
        }
    }

    impl FakeIpRunner {
        fn with_responses<const N: usize>(responses: [IpOutput; N]) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                calls: Mutex::new(Vec::new()),
                #[cfg(target_os = "linux")]
                unavailable_tool: None,
                #[cfg(target_os = "linux")]
                network_admin: true,
            }
        }

        #[cfg(target_os = "linux")]
        fn without_tool(tool: &str) -> Self {
            Self {
                unavailable_tool: Some(tool.to_string()),
                ..Self::default()
            }
        }

        #[cfg(target_os = "linux")]
        fn without_admin() -> Self {
            Self {
                network_admin: false,
                ..Self::default()
            }
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().expect("calls lock").clone()
        }
    }

    #[async_trait]
    impl IpCommandRunner for FakeIpRunner {
        async fn output(&self, args: &[String], _timeout: Duration) -> Result<IpOutput> {
            self.calls.lock().expect("calls lock").push(args.to_vec());
            Ok(self
                .responses
                .lock()
                .expect("responses lock")
                .pop_front()
                .unwrap_or_else(|| success(b"")))
        }

        #[cfg(target_os = "linux")]
        fn executable_in_path(&self, name: &str) -> bool {
            self.unavailable_tool.as_deref() != Some(name)
        }

        #[cfg(target_os = "linux")]
        fn has_network_admin(&self) -> bool {
            self.network_admin
        }
    }

    fn success(stdout: &[u8]) -> IpOutput {
        IpOutput {
            success: true,
            status: "exit status: 0".to_string(),
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
        }
    }

    fn failure(stderr: &str) -> IpOutput {
        IpOutput {
            success: false,
            status: "exit status: 1".to_string(),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }
}
