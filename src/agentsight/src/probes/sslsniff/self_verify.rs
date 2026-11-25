//! 静态 SSL 偏移验证：有界信号结转与轮换目标校验。

use super::late_retry::ProcessIdentity;
use super::{FileKey, StaticSslOffsets, parse_maps_line, stat_dev_to_key};
use crate::probes::codex_offsets::Fingerprint;
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, BufRead},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    time::{Duration, Instant},
};

pub(super) const VERIFY_WINDOW: Duration = Duration::from_secs(30);
const MAX_SIGNALS: usize = 1024;

#[derive(Clone, Copy, Default)]
struct Directions {
    request: Option<Instant>,
    response: Option<Instant>,
}

impl Directions {
    fn merge(&mut self, other: Self) {
        self.request = self.request.max(other.request);
        self.response = self.response.max(other.response);
    }

    fn fold_into(self, pending: &mut PendingVerify) {
        // 轮换或重新挂载前的流量不能确认当前候选。
        pending.saw_request |= self.request.is_some_and(|at| at >= pending.attached_at);
        pending.saw_response |= self.response.is_some_and(|at| at >= pending.attached_at);
    }
}

/// 生产侧只记录方向与单调时钟，不在 ring-buffer 回调里访问 procfs。
#[derive(Default)]
pub(crate) struct PlaintextSignals {
    entries: HashMap<u32, Directions>,
}

impl PlaintextSignals {
    pub(crate) fn record(&mut self, pid: u32, request: bool, response: bool) {
        self.record_at(pid, request, response, Instant::now());
    }

    fn record_at(&mut self, pid: u32, request: bool, response: bool, now: Instant) {
        if !request && !response {
            return;
        }
        if self.entries.len() >= MAX_SIGNALS && !self.entries.contains_key(&pid) {
            return;
        }
        self.entries.entry(pid).or_default().merge(Directions {
            request: request.then_some(now),
            response: response.then_some(now),
        });
    }

    pub(super) fn forget(&mut self, pid: u32) {
        self.entries.remove(&pid);
    }
}

struct DeferredSignal {
    directions: Directions,
    identity: ProcessIdentity,
    expires_at: Instant,
}

/// 仅为尚未完成 exec 归因的信号保留一个有界验证窗口。
#[derive(Default)]
pub(super) struct SignalBacklog {
    entries: HashMap<u32, DeferredSignal>,
}

impl SignalBacklog {
    pub(super) fn forget(&mut self, pid: u32) {
        self.entries.remove(&pid);
    }

    pub(super) fn fold(
        &mut self,
        incoming: PlaintextSignals,
        pending: &mut HashMap<FileKey, PendingVerify>,
        pid_inodes: &HashMap<u32, Vec<FileKey>>,
        identities: &HashMap<u32, ProcessIdentity>,
        now: Instant,
        mut read_identity: impl FnMut(u32) -> Option<ProcessIdentity>,
    ) {
        self.entries.retain(|_, signal| signal.expires_at > now);
        if pending.is_empty() {
            self.entries.clear();
            return;
        }
        for (pid, directions) in incoming.entries {
            if let Some(files) = pid_inodes.get(&pid).filter(|files| !files.is_empty()) {
                for file in files {
                    if let Some(p) = pending.get_mut(file) {
                        directions.fold_into(p);
                    }
                }
                continue;
            }
            let Some(identity) = identities.get(&pid).copied().or_else(|| read_identity(pid))
            else {
                continue;
            };
            if self
                .entries
                .get(&pid)
                .is_some_and(|signal| signal.identity != identity)
            {
                self.entries.remove(&pid);
            }
            if self.entries.len() >= MAX_SIGNALS && !self.entries.contains_key(&pid) {
                continue;
            }
            let signal = self.entries.entry(pid).or_insert(DeferredSignal {
                directions: Directions::default(),
                identity,
                expires_at: now + VERIFY_WINDOW,
            });
            // 后续响应不延长首个未归因信号的 TTL。
            signal.directions.merge(directions);
        }
        self.entries.retain(|pid, signal| {
            let Some(files) = pid_inodes.get(pid).filter(|files| !files.is_empty()) else {
                return true;
            };
            // PID 复用不能把上一进程的请求带给新进程。
            if identities.get(pid) == Some(&signal.identity) {
                for file in files {
                    if let Some(p) = pending.get_mut(file) {
                        signal.directions.fold_into(p);
                    }
                }
            }
            false
        });
    }
}

pub(super) type FileRevision = (u64, i64, i64);

pub(super) fn file_revision(path: &str) -> Option<FileRevision> {
    fs::metadata(path)
        .ok()
        .map(|m| (m.len(), m.mtime(), m.mtime_nsec()))
}

/// 从固定描述符派生内核映射身份；overlayfs 的路径 stat 身份可能不同。
fn mapped_file_key(file: &File) -> io::Result<FileKey> {
    // PROT_NONE 不读取任何字节；一页临时 VMA 只让内核选择 mmap 的真实文件。
    // SAFETY: 有效只读文件描述符，内核选择地址；长度非零，不解引用返回地址。
    let address = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            1,
            libc::PROT_NONE,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        )
    };
    if address == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    struct Mapping(*mut libc::c_void);
    impl Drop for Mapping {
        fn drop(&mut self) {
            // SAFETY: 仅释放本函数成功创建、尚未释放的一字节映射。
            unsafe {
                libc::munmap(self.0, 1);
            }
        }
    }
    let mapping = Mapping(address);
    let address = mapping.0 as usize as u64;
    // 此处读观察者自己的 VMA，不能用配置的宿主 procfs 或目标 PID。
    let maps = io::BufReader::new(File::open("/proc/self/maps")?);
    for line in maps.lines() {
        let line = line?;
        if let Some(((start, end), key, _)) = parse_maps_line(&line)
            && start <= address
            && address < end
        {
            return Ok(key);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "temporary SSL identity mapping is missing",
    ))
}

/// 固定目标进程命名空间内的映射文件，拒绝同名文件替换或设备不匹配。
pub(super) fn open_uprobe_target(path: &str, expected: FileKey) -> io::Result<File> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    let observed = (stat_dev_to_key(metadata.dev()), metadata.ino());
    if observed != expected && mapped_file_key(&file)? != expected {
        // 仍比较完整设备/inode，不能仅凭 inode 相同放行：overlayfs 可在
        // stat 时返回叠加层设备、mmap 时返回底层设备，两个身份都来自同一 fd。
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "SSL uprobe target identity changed: expected {expected:?}, observed {observed:?}"
            ),
        ));
    }
    Ok(file)
}

/// 先固定描述符，再校验 inode 和内容修订，避免检查后路径被自更新替换。
pub(super) fn open_rotation_target(
    path: &str,
    expected: FileKey,
    revision: Option<FileRevision>,
) -> io::Result<File> {
    let file = open_uprobe_target(path, expected)?;
    let metadata = file.metadata()?;
    if Some((metadata.len(), metadata.mtime(), metadata.mtime_nsec())) != revision {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "SSL rotation target changed",
        ));
    }
    Ok(file)
}

pub(super) fn pinned_path(file: &File) -> String {
    format!("/proc/self/fd/{}", file.as_raw_fd())
}

/// 候选偏移与挂载时的内容身份；验证只确认当前挂载之后采集的请求。
pub(super) struct PendingVerify {
    pub(super) candidates: Vec<StaticSslOffsets>,
    pub(super) index: usize,
    pub(super) path: String,
    pub(super) attached_at: Instant,
    pub(super) saw_request: bool,
    pub(super) saw_response: bool,
    pub(super) fingerprint: Option<Fingerprint>,
    pub(super) revision: Option<FileRevision>,
    /// 告警只发一次，仍允许后续请求确认最后一个候选。
    pub(super) exhausted: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    Verified,
    Rotate,
    Exhausted,
    Idle,
}

pub(super) fn classify_pending_verdicts(
    pending: &HashMap<FileKey, PendingVerify>,
    now: Instant,
) -> HashMap<FileKey, Verdict> {
    let mut verdicts = HashMap::new();
    for (file, p) in pending {
        if p.saw_request {
            verdicts.insert(*file, Verdict::Verified);
        } else if now.duration_since(p.attached_at) >= VERIFY_WINDOW {
            let verdict = if p.saw_response && !p.exhausted {
                if p.index + 1 < p.candidates.len() {
                    Verdict::Rotate
                } else {
                    Verdict::Exhausted
                }
            } else {
                Verdict::Idle
            };
            verdicts.insert(*file, verdict);
        }
    }
    verdicts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(pid: u32) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            start_time: 1000 + u64::from(pid),
        }
    }

    fn pending(at: Instant, candidates: usize) -> HashMap<FileKey, PendingVerify> {
        HashMap::from([(
            (1, 1),
            PendingVerify {
                candidates: (0..candidates)
                    .map(|ssl_write| StaticSslOffsets {
                        ssl_write,
                        ssl_read: 2,
                        ssl_do_handshake: 3,
                        write_is_ex: false,
                        read_is_ex: false,
                    })
                    .collect(),
                index: 0,
                path: "fixture".into(),
                attached_at: at,
                saw_request: false,
                saw_response: false,
                fingerprint: None,
                revision: None,
                exhausted: false,
            },
        )])
    }

    #[test]
    fn request_survives_multiple_polls_before_exec_association() {
        let now = Instant::now();
        let mut pending = pending(now, 2);
        let mut backlog = SignalBacklog::default();
        let mut signal = PlaintextSignals::default();
        signal.record_at(42, true, false, now);
        backlog.fold(
            signal,
            &mut pending,
            &HashMap::new(),
            &HashMap::new(),
            now,
            |pid| Some(identity(pid)),
        );
        for i in 1..5 {
            backlog.fold(
                PlaintextSignals::default(),
                &mut pending,
                &HashMap::new(),
                &HashMap::new(),
                now + Duration::from_millis(i),
                |_| None,
            );
        }
        let mut response = PlaintextSignals::default();
        response.record_at(42, false, true, now + Duration::from_secs(1));
        backlog.fold(
            response,
            &mut pending,
            &HashMap::from([(42, vec![(1, 1)])]),
            &HashMap::from([(42, identity(42))]),
            now + Duration::from_secs(1),
            |_| None,
        );
        assert_eq!(
            classify_pending_verdicts(&pending, now + VERIFY_WINDOW)[&(1, 1)],
            Verdict::Verified
        );
        assert!(backlog.entries.is_empty());
    }

    #[test]
    fn deferred_signals_expire_and_do_not_validate_reused_pids() {
        for (delay, start_time) in [(VERIFY_WINDOW, 1042), (Duration::from_secs(1), 9999)] {
            let now = Instant::now();
            let mut pending = pending(now, 1);
            let mut backlog = SignalBacklog::default();
            let mut signal = PlaintextSignals::default();
            signal.record_at(42, true, false, now);
            backlog.fold(
                signal,
                &mut pending,
                &HashMap::new(),
                &HashMap::new(),
                now,
                |pid| Some(identity(pid)),
            );
            backlog.fold(
                PlaintextSignals::default(),
                &mut pending,
                &HashMap::from([(42, vec![(1, 1)])]),
                &HashMap::from([(
                    42,
                    ProcessIdentity {
                        pid: 42,
                        start_time,
                    },
                )]),
                now + delay,
                |_| None,
            );
            assert!(!pending[&(1, 1)].saw_request);
            assert!(backlog.entries.is_empty());
        }
    }

    #[test]
    fn pre_attach_signals_cannot_confirm_a_rotated_candidate() {
        let now = Instant::now();
        let mut pending = pending(now + Duration::from_secs(1), 1);
        let mut signal = PlaintextSignals::default();
        signal.record_at(42, true, true, now);
        SignalBacklog::default().fold(
            signal,
            &mut pending,
            &HashMap::from([(42, vec![(1, 1)])]),
            &HashMap::new(),
            now + Duration::from_secs(1),
            |_| None,
        );
        assert!(!pending[&(1, 1)].saw_request && !pending[&(1, 1)].saw_response);
    }

    #[test]
    fn signals_are_bounded_on_both_sides_and_drained_without_pending() {
        let now = Instant::now();
        let mut signal = PlaintextSignals::default();
        for pid in 1..=(MAX_SIGNALS as u32 + 10) {
            signal.record_at(pid, true, false, now);
        }
        assert_eq!(signal.entries.len(), MAX_SIGNALS);
        let mut backlog = SignalBacklog::default();
        let mut pending = pending(now, 1);
        backlog.fold(
            signal,
            &mut pending,
            &HashMap::new(),
            &HashMap::new(),
            now,
            |pid| Some(identity(pid)),
        );
        let mut extra = PlaintextSignals::default();
        extra.record_at(9999, true, false, now);
        backlog.fold(
            extra,
            &mut pending,
            &HashMap::new(),
            &HashMap::new(),
            now,
            |pid| Some(identity(pid)),
        );
        assert_eq!(backlog.entries.len(), MAX_SIGNALS);
        pending.clear();
        backlog.fold(
            PlaintextSignals::default(),
            &mut pending,
            &HashMap::new(),
            &HashMap::new(),
            now,
            |_| None,
        );
        assert!(backlog.entries.is_empty());
    }

    #[test]
    fn verdicts_rotate_exhaust_wait_and_accept_late_requests() {
        let now = Instant::now();
        let mut pending = pending(now, 2);
        assert!(classify_pending_verdicts(&pending, now).is_empty());
        assert_eq!(
            classify_pending_verdicts(&pending, now + VERIFY_WINDOW)[&(1, 1)],
            Verdict::Idle
        );
        pending.get_mut(&(1, 1)).unwrap().saw_response = true;
        assert_eq!(
            classify_pending_verdicts(&pending, now + VERIFY_WINDOW)[&(1, 1)],
            Verdict::Rotate
        );
        pending.get_mut(&(1, 1)).unwrap().index = 1;
        assert_eq!(
            classify_pending_verdicts(&pending, now + VERIFY_WINDOW)[&(1, 1)],
            Verdict::Exhausted
        );
        pending.get_mut(&(1, 1)).unwrap().exhausted = true;
        assert_eq!(
            classify_pending_verdicts(&pending, now + VERIFY_WINDOW)[&(1, 1)],
            Verdict::Idle
        );
        pending.get_mut(&(1, 1)).unwrap().saw_request = true;
        assert_eq!(
            classify_pending_verdicts(&pending, now + VERIFY_WINDOW)[&(1, 1)],
            Verdict::Verified
        );
    }

    #[test]
    #[ignore = "需 overlayfs 容器，且 /usr/bin/python3 来自镜像底层"]
    fn overlay_lower_file_is_pinned_without_dropping_device_checks() {
        let path = "/usr/bin/python3";
        let file = File::open(path).unwrap();
        let metadata = file.metadata().unwrap();
        let stat_key = (stat_dev_to_key(metadata.dev()), metadata.ino());
        let mapped_key = mapped_file_key(&file).unwrap();
        assert_ne!(stat_key, mapped_key, "夹具必须实际覆盖 overlay 的身份分叉");
        let target = open_uprobe_target(path, mapped_key).unwrap();
        assert_eq!(mapped_file_key(&target).unwrap(), mapped_key);
        let wrong_device = mapped_key.0 ^ (1u64 << 32);
        assert!(open_uprobe_target(path, (wrong_device, mapped_key.1)).is_err());
        assert!(open_uprobe_target(path, (mapped_key.0, mapped_key.1 + 1)).is_err());
        eprintln!("overlay 路径身份 {stat_key:?}，实际映射身份 {mapped_key:?} 校验通过");
    }

    #[test]
    fn uprobe_target_pins_identity_and_rejects_replaced_or_wrong_device_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("libssl.so");
        fs::write(&path, b"mapped library").unwrap();
        let path = path.to_str().unwrap();
        let metadata = fs::metadata(path).unwrap();
        let key = (stat_dev_to_key(metadata.dev()), metadata.ino());
        assert!(open_uprobe_target(path, (key.0 + 1, key.1)).is_err());
        assert!(open_uprobe_target(path, (key.0, key.1 + 1)).is_err());
        let target = open_uprobe_target(path, key).unwrap();
        let pinned = pinned_path(&target);
        let replacement = dir.path().join("replacement");
        fs::write(&replacement, b"different library").unwrap();
        fs::rename(&replacement, path).unwrap();
        assert!(open_uprobe_target(path, key).is_err());
        assert_eq!(fs::read(&pinned).unwrap(), b"mapped library");
        drop(target);
        assert!(open_uprobe_target(dir.path().join("missing").to_str().unwrap(), key).is_err());
    }

    #[test]
    fn pinned_mapping_identity_matches_the_kernel_and_is_released() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library");
        fs::write(&path, b"mapped fixture").unwrap();
        let file = File::open(&path).unwrap();
        let expected = mapped_file_key(&file).unwrap();
        let target = open_uprobe_target(path.to_str().unwrap(), expected).unwrap();
        assert_eq!(mapped_file_key(&target).unwrap(), expected);
        assert!(open_uprobe_target(path.to_str().unwrap(), (expected.0 + 1, expected.1)).is_err());
        assert!(open_uprobe_target(path.to_str().unwrap(), (expected.0, expected.1 + 1)).is_err());
        let replacement = dir.path().join("replacement");
        fs::write(&replacement, b"replacement").unwrap();
        fs::rename(&replacement, &path).unwrap();
        assert!(open_uprobe_target(path.to_str().unwrap(), expected).is_err());
        assert_eq!(mapped_file_key(&target).unwrap(), expected);
        let maps = fs::read_to_string("/proc/self/maps").unwrap();
        assert!(
            !maps
                .lines()
                .any(|line| parse_maps_line(line).is_some_and(|(_, key, _)| key == expected))
        );
    }

    #[test]
    fn rotation_rejects_replaced_or_rewritten_files_and_pins_valid_inode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent");
        fs::write(&path, b"old binary").unwrap();
        let path = path.to_str().unwrap();
        let meta = fs::metadata(path).unwrap();
        let key = (stat_dev_to_key(meta.dev()), meta.ino());
        let revision = file_revision(path);
        let pinned = open_rotation_target(path, key, revision).unwrap();
        assert!(open_rotation_target(path, key, None).is_err());
        fs::write(path, b"rewritten binary, different size").unwrap();
        assert!(open_rotation_target(path, key, revision).is_err());
        let replacement = dir.path().join("new-agent");
        fs::write(&replacement, b"new binary").unwrap();
        fs::rename(replacement, path).unwrap();
        assert!(open_rotation_target(path, key, revision).is_err());
        let pinned_meta = fs::metadata(pinned_path(&pinned)).unwrap();
        assert_eq!((stat_dev_to_key(pinned_meta.dev()), pinned_meta.ino()), key);
        assert!(
            open_rotation_target(dir.path().join("missing").to_str().unwrap(), key, revision)
                .is_err()
        );
    }
}
