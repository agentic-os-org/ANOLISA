//! Resolves process identity through a task iterator when procfs hides outer PIDs.

use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;

use libbpf_rs::{Iter, ObjectBuilder};

static OBJECT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/target_identity.bpf.o"));

const RECORD_SIZE: usize = 24;
const MAX_ITERATOR_BYTES: u64 = 16 * 1024 * 1024;
const NANOS_PER_SECOND: i128 = 1_000_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TargetRecord {
    kernel_pid: i32,
    local_pid: i32,
    pidns_inum: u32,
    start_boottime: u64,
}

/// Kernel process identity and the procfs clock bias validated during resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessIdentity {
    /// Init-namespace kernel TGID.
    pub kernel_pid: i32,
    /// Namespace-init ticks subtracted by procfs in container uptime mode.
    pub procfs_start_time_bias: u64,
}

/// Resolves a namespace-local process ID to its init-namespace kernel identity.
///
/// The task iterator enumerates processes visible in the caller's active PID
/// namespace. The target's namespace inode and procfs start time must both
/// match, so a colliding or reused local PID is never accepted as identity.
/// PID 1 cross-checks whether this procfs subtracts namespace-init ticks, as
/// Alibaba kernels do for containers in a non-root `cpuacct` cgroup.
///
/// # Errors
///
/// Returns `NotFound` when the target is absent or its start time no longer
/// matches. Returns another I/O error when the namespace cannot be inspected,
/// procfs and iterator clocks disagree, or the BPF iterator cannot be used safely.
pub fn resolve_process_identity(
    local_pid: i32,
    expected_start_time: u64,
) -> io::Result<ProcessIdentity> {
    if local_pid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "target PID must be positive",
        ));
    }
    let namespace = fs::metadata(format!("/proc/{local_pid}/ns/pid"))?;
    let caller_namespace = fs::metadata("/proc/self/ns/pid")?;
    let proc_init_namespace = fs::metadata("/proc/1/ns/pid")?;
    if caller_namespace.dev() != proc_init_namespace.dev()
        || caller_namespace.ino() != proc_init_namespace.ino()
    {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "procfs PID 1 is not the caller's PID namespace init",
        ));
    }
    let proc_init_start = read_procfs_start_time(1)?;
    let boottime_offset = read_boottime_offset()?;
    let clock_ticks = clock_ticks_per_second()?;
    let records = read_records()?;
    boottime_offset.verify()?;
    let procfs_start_time_bias = select_procfs_start_time_bias(
        &records,
        caller_namespace.ino(),
        proc_init_start,
        boottime_offset.value,
        clock_ticks,
    )?;
    let kernel_pid = select_kernel_pid(
        &records,
        local_pid,
        namespace.ino(),
        expected_start_time,
        boottime_offset.value,
        clock_ticks,
        procfs_start_time_bias,
    )?;
    Ok(ProcessIdentity {
        kernel_pid,
        procfs_start_time_bias,
    })
}

/// Converts kernel `start_boottime` nanoseconds using the validated procfs bias.
///
/// # Errors
///
/// Returns an error when the caller's time namespace is unstable or differs
/// between the current thread, process leader, and child namespace, or when the
/// result is outside the procfs clock-tick range.
pub fn procfs_start_time_from_boottime(
    start_boottime: u64,
    start_time_bias: u64,
) -> io::Result<u64> {
    let boottime_offset = read_boottime_offset()?;
    let clock_ticks = clock_ticks_per_second()?;
    boottime_offset.verify()?;
    proc_start_time_with_bias(
        start_boottime,
        boottime_offset.value,
        clock_ticks,
        start_time_bias,
    )
    .ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "process start time is outside the procfs clock-tick representation",
        )
    })
}

pub(crate) fn kernel_process_start_ticks(kernel_pid: i32) -> io::Result<Option<u64>> {
    if kernel_pid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "kernel PID must be positive",
        ));
    }
    let namespace = fs::metadata("/proc/self/ns/pid")?;
    let clock_ticks = clock_ticks_per_second()?;
    let records = read_records()?;
    select_kernel_process_start_ticks(&records, kernel_pid, namespace.ino(), clock_ticks)
}

fn read_records() -> io::Result<Vec<TargetRecord>> {
    let open = ObjectBuilder::default()
        .open_memory(OBJECT)
        .map_err(|error| iterator_error("open target identity object", error))?;
    let mut object = open
        .load()
        .map_err(|error| iterator_error("load target identity object", error))?;
    let program = object.prog_mut("resolve_target_identity").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "target identity object has no resolve_target_identity program",
        )
    })?;
    let link = program
        .attach()
        .map_err(|error| iterator_error("attach target identity iterator", error))?;
    let iter = Iter::new(&link)
        .map_err(|error| iterator_error("create target identity iterator", error))?;
    let mut bytes = Vec::new();
    iter.take(MAX_ITERATOR_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| iterator_error("read target identity iterator", error))?;
    if bytes.len() as u64 > MAX_ITERATOR_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "target identity iterator output exceeds 16 MiB",
        ));
    }
    decode_records(&bytes)
}

fn iterator_error(context: &str, error: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("{context}: {error}"))
}

fn read_procfs_start_time(pid: i32) -> io::Result<u64> {
    let path = format!("/proc/{pid}/stat");
    let bytes = fs::read(&path)?;
    let stat = String::from_utf8_lossy(&bytes);
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{path} has no valid field 22 start time"),
        )
    };
    stat.rsplit_once(") ")
        .ok_or_else(invalid)?
        .1
        .split_whitespace()
        .nth(19)
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())
}

fn decode_records(bytes: &[u8]) -> io::Result<Vec<TargetRecord>> {
    if bytes.len() % RECORD_SIZE != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "target identity iterator returned a partial record",
        ));
    }
    bytes
        .chunks_exact(RECORD_SIZE)
        .map(|record| {
            Ok(TargetRecord {
                kernel_pid: i32::from_ne_bytes(record[0..4].try_into().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid kernel PID record")
                })?),
                local_pid: i32::from_ne_bytes(record[4..8].try_into().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid local PID record")
                })?),
                pidns_inum: u32::from_ne_bytes(record[8..12].try_into().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid PID namespace record")
                })?),
                start_boottime: u64::from_ne_bytes(record[16..24].try_into().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid process start-time record",
                    )
                })?),
            })
        })
        .collect()
}

fn select_procfs_start_time_bias(
    records: &[TargetRecord],
    pidns_inum: u64,
    proc_init_start: u64,
    boottime_offset: i128,
    clock_ticks: u64,
) -> io::Result<u64> {
    let mut candidates = records
        .iter()
        .filter(|record| record.local_pid == 1 && u64::from(record.pidns_inum) == pidns_inum);
    let Some(init) = candidates.next() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("PID namespace {pidns_inum} has no visible init process"),
        ));
    };
    if candidates.next().is_some() || init.kernel_pid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("PID namespace {pidns_inum} has ambiguous init identity records"),
        ));
    }
    let init_start = proc_start_time(init.start_boottime, boottime_offset, clock_ticks)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "namespace init start time overflows the procfs clock-tick representation",
            )
        })?;
    match proc_init_start {
        0 if init_start != 0 => Ok(init_start),
        value if value == init_start && value != 0 => Ok(0),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "procfs PID 1 start {proc_init_start} does not match iterator start {init_start} in either clock mode"
            ),
        )),
    }
}

fn select_kernel_pid(
    records: &[TargetRecord],
    local_pid: i32,
    pidns_inum: u64,
    expected_start_time: u64,
    boottime_offset: i128,
    clock_ticks: u64,
    start_time_bias: u64,
) -> io::Result<i32> {
    let mut candidates = records.iter().filter(|record| {
        record.local_pid == local_pid && u64::from(record.pidns_inum) == pidns_inum
    });
    let Some(candidate) = candidates.next() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("process {local_pid} is absent from PID namespace {pidns_inum}"),
        ));
    };
    if candidates.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("process {local_pid} has ambiguous kernel identity records"),
        ));
    }
    if candidate.kernel_pid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("process {local_pid} has an invalid kernel PID"),
        ));
    }
    let actual_start = proc_start_time_with_bias(
        candidate.start_boottime,
        boottime_offset,
        clock_ticks,
        start_time_bias,
    )
    .ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "target process start time is outside the procfs clock-tick representation",
        )
    })?;
    if actual_start != expected_start_time {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("process {local_pid} is stale or has been reused"),
        ));
    }
    Ok(candidate.kernel_pid)
}

fn select_kernel_process_start_ticks(
    records: &[TargetRecord],
    kernel_pid: i32,
    pidns_inum: u64,
    clock_ticks: u64,
) -> io::Result<Option<u64>> {
    let mut candidates = records
        .iter()
        .filter(|record| record.kernel_pid == kernel_pid);
    if let Some(candidate) = candidates.next() {
        if candidates.next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("kernel process {kernel_pid} has ambiguous identity records"),
            ));
        }
        return proc_start_time(candidate.start_boottime, 0, clock_ticks)
            .map(Some)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "kernel process start time overflows the clock-tick representation",
                )
            });
    }

    // Global PID 1 proves that this iterator sees the initial PID namespace.
    // Otherwise absence can also mean a live process in an invisible namespace.
    if records.iter().any(|record| {
        record.kernel_pid == 1
            && record.local_pid == 1
            && u64::from(record.pidns_inum) == pidns_inum
    }) {
        return Ok(None);
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        format!("kernel process {kernel_pid} is not visible from this PID namespace"),
    ))
}

fn proc_start_time(start_boottime: u64, boottime_offset: i128, clock_ticks: u64) -> Option<u64> {
    if clock_ticks == 0 || NANOS_PER_SECOND % i128::from(clock_ticks) != 0 {
        return None;
    }
    let adjusted = i128::from(start_boottime).checked_add(boottime_offset)?;
    if adjusted < 0 || adjusted > i128::from(u64::MAX) {
        return None;
    }
    let divisor = NANOS_PER_SECOND / i128::from(clock_ticks);
    u64::try_from(adjusted / divisor).ok()
}

fn proc_start_time_with_bias(
    start_boottime: u64,
    boottime_offset: i128,
    clock_ticks: u64,
    start_time_bias: u64,
) -> Option<u64> {
    // Alibaba's container uptime patch subtracts init ticks after each operand
    // has been truncated, not the raw nanosecond delta.
    proc_start_time(start_boottime, boottime_offset, clock_ticks)?.checked_sub(start_time_bias)
}

type NamespaceIdentity = (u64, u64);

struct BoottimeOffset {
    value: i128,
    namespace: Option<NamespaceIdentity>,
}

impl BoottimeOffset {
    fn verify(&self) -> io::Result<()> {
        for path in [
            "/proc/thread-self/ns/time",
            "/proc/self/ns/time",
            "/proc/thread-self/ns/time_for_children",
            "/proc/self/ns/time_for_children",
        ] {
            if namespace_identity(path)? != self.namespace {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "time namespace changed while resolving process identity",
                ));
            }
        }
        Ok(())
    }
}

fn namespace_identity(path: &'static str) -> io::Result<Option<NamespaceIdentity>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some((metadata.dev(), metadata.ino()))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // CONFIG_TIME_NS=n has no time entries; an unreadable procfs is not absent support.
            fs::read_dir("/proc/thread-self/ns")?;
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn read_boottime_offset() -> io::Result<BoottimeOffset> {
    let current = namespace_identity("/proc/thread-self/ns/time")?;
    let leader = namespace_identity("/proc/self/ns/time")?;
    let current_children = namespace_identity("/proc/thread-self/ns/time_for_children")?;
    let leader_children = namespace_identity("/proc/self/ns/time_for_children")?;
    // The offset file is meaningful only when both threads and both child namespaces agree.
    if current != leader || current != current_children || current != leader_children {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "current, leader, and child time namespaces differ",
        ));
    }
    let value = if current.is_some() {
        parse_boottime_offset(&fs::read_to_string("/proc/self/timens_offsets")?)?
    } else {
        0
    };
    Ok(BoottimeOffset {
        value,
        namespace: current,
    })
}

fn parse_boottime_offset(contents: &str) -> io::Result<i128> {
    let Some(line) = contents.lines().find(|line| line.starts_with("boottime")) else {
        return Ok(0);
    };
    let fields: Vec<_> = line.split_whitespace().collect();
    if fields.len() != 3 {
        return Err(invalid_time_offset());
    }
    let seconds = fields[1]
        .parse::<i64>()
        .map_err(|_| invalid_time_offset())?;
    let nanoseconds = fields[2]
        .parse::<i64>()
        .map_err(|_| invalid_time_offset())?;
    if !(0..1_000_000_000).contains(&nanoseconds) {
        return Err(invalid_time_offset());
    }
    i128::from(seconds)
        .checked_mul(NANOS_PER_SECOND)
        .and_then(|offset| offset.checked_add(i128::from(nanoseconds)))
        .ok_or_else(invalid_time_offset)
}

fn invalid_time_offset() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid /proc/self/timens_offsets boottime entry",
    )
}

fn clock_ticks_per_second() -> io::Result<u64> {
    // SAFETY: sysconf takes a constant key and has no pointer arguments.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks <= 0 {
        return Err(io::Error::other(
            "sysconf(_SC_CLK_TCK) did not return a positive clock frequency",
        ));
    }
    Ok(ticks as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(kernel_pid: i32, local_pid: i32, pidns_inum: u32, start: u64) -> TargetRecord {
        TargetRecord {
            kernel_pid,
            local_pid,
            pidns_inum,
            start_boottime: start,
        }
    }

    fn encode(record: TargetRecord) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&record.kernel_pid.to_ne_bytes());
        bytes.extend_from_slice(&record.local_pid.to_ne_bytes());
        bytes.extend_from_slice(&record.pidns_inum.to_ne_bytes());
        bytes.extend_from_slice(&0u32.to_ne_bytes());
        bytes.extend_from_slice(&record.start_boottime.to_ne_bytes());
        bytes
    }

    #[test]
    fn decodes_native_records() {
        let expected = record(4100, 17, 4026533000, 123_000_000);
        let decoded = decode_records(&encode(expected)).expect("decode record");
        assert_eq!(decoded, vec![expected]);
    }

    #[test]
    fn rejects_partial_records() {
        let error = decode_records(&[0; RECORD_SIZE - 1]).expect_err("partial record must fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn resolves_only_matching_namespace_and_start_time() {
        let records = [
            record(4000, 17, 1000, 50_000_000),
            record(4100, 17, 2000, 123_000_000),
            record(4200, 18, 2000, 123_000_000),
        ];
        let resolved =
            select_kernel_pid(&records, 17, 2000, 12, 0, 100, 0).expect("resolve target");
        assert_eq!(resolved, 4100);
    }

    #[test]
    fn calibrates_container_tick_bias_from_pid_one() {
        let records = [
            record(320, 1, 2000, 158_539_519_520),
            record(348, 14, 2000, 161_602_832_594),
        ];
        let bias =
            select_procfs_start_time_bias(&records, 2000, 0, 0, 100).expect("container clock mode");
        assert_eq!(bias, 15853);
        assert_eq!(
            select_kernel_pid(&records, 14, 2000, 307, 0, 100, bias)
                .expect("resolve container target"),
            348
        );
    }

    #[test]
    fn container_bias_subtracts_ticks_after_truncating_each_operand() {
        let records = [
            record(320, 1, 2000, 19_999_999),
            record(348, 14, 2000, 20_000_000),
        ];
        let bias =
            select_procfs_start_time_bias(&records, 2000, 0, 0, 100).expect("container clock mode");
        assert_eq!(bias, 1);
        assert_eq!(
            select_kernel_pid(&records, 14, 2000, 1, 0, 100, bias)
                .expect("tick subtraction must not use the one-nanosecond delta"),
            348
        );
    }

    #[test]
    fn accepts_unbiased_procfs_only_when_pid_one_matches() {
        let records = [record(320, 1, 2000, 123_000_000)];
        assert_eq!(
            select_procfs_start_time_bias(&records, 2000, 12, 0, 100)
                .expect("ordinary procfs clock"),
            0
        );
        for proc_init_start in [11, 13] {
            let error = select_procfs_start_time_bias(&records, 2000, proc_init_start, 0, 100)
                .expect_err("inconsistent init clocks must fail closed");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
        let zero_records = [record(320, 1, 2000, 1_000_000)];
        let error = select_procfs_start_time_bias(&zero_records, 2000, 0, 0, 100)
            .expect_err("a zero init tick cannot distinguish clock modes");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn rejects_stale_and_missing_targets() {
        let records = [record(4100, 17, 2000, 123_000_000)];
        let stale = select_kernel_pid(&records, 17, 2000, 11, 0, 100, 0)
            .expect_err("start-time mismatch must fail");
        assert_eq!(stale.kind(), io::ErrorKind::NotFound);
        let missing = select_kernel_pid(&records, 19, 2000, 12, 0, 100, 0)
            .expect_err("missing PID must fail");
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn rejects_ambiguous_records() {
        let records = [
            record(4100, 17, 2000, 123_000_000),
            record(4200, 17, 2000, 123_000_000),
        ];
        let error = select_kernel_pid(&records, 17, 2000, 12, 0, 100, 0)
            .expect_err("duplicate identity must fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn rejects_non_positive_kernel_pids() {
        for kernel_pid in [0, -1] {
            let records = [record(kernel_pid, 17, 2000, 123_000_000)];
            let error = select_kernel_pid(&records, 17, 2000, 12, 0, 100, 0)
                .expect_err("invalid kernel PID must fail");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn resolves_kernel_process_start_without_a_time_namespace_offset() {
        let records = [
            record(1, 1, 1000, 1_000_000),
            record(4100, 17, 2000, 123_000_000),
        ];
        assert_eq!(
            select_kernel_process_start_ticks(&records, 4100, 2000, 100).expect("live process"),
            Some(12)
        );
        assert_eq!(
            select_kernel_process_start_ticks(&records, 4200, 1000, 100).expect("stale process"),
            None
        );
    }

    #[test]
    fn refuses_to_call_an_invisible_kernel_process_stale() {
        let records = [record(4100, 1, 2000, 123_000_000)];
        let error = select_kernel_process_start_ticks(&records, 4200, 2000, 100)
            .expect_err("an invisible process may still be live");
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    }

    #[test]
    fn rejects_duplicate_kernel_process_records() {
        let records = [
            record(1, 1, 1000, 1_000_000),
            record(4100, 17, 2000, 123_000_000),
            record(4100, 23, 3000, 123_000_000),
        ];
        let error = select_kernel_process_start_ticks(&records, 4100, 1000, 100)
            .expect_err("duplicate kernel identity must fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn applies_boottime_namespace_offset_before_tick_conversion() {
        assert_eq!(proc_start_time(123_000_000, -3_000_000, 100), Some(12));
        assert_eq!(proc_start_time(1_000_000, -3_000_000, 100), None);
        assert_eq!(proc_start_time(123_000_000, 0, 0), None);
    }

    #[test]
    fn parses_signed_boottime_offsets() {
        assert_eq!(
            parse_boottime_offset("monotonic 1 2\nboottime -3 4\n").expect("offset"),
            -2_999_999_996
        );
        assert_eq!(
            parse_boottime_offset("monotonic 1 2\n").expect("default"),
            0
        );
        assert!(parse_boottime_offset("boottime 0 1000000000\n").is_err());
        assert!(parse_boottime_offset("boottime malformed\n").is_err());
    }

    fn target_start_time(local_pid: i32) -> u64 {
        fs::read_to_string(format!("/proc/{local_pid}/stat"))
            .expect("target stat")
            .rsplit_once(") ")
            .expect("comm terminator")
            .1
            .split_whitespace()
            .nth(19)
            .expect("start time")
            .parse()
            .expect("numeric start time")
    }

    #[test]
    #[ignore = "requires root/CAP_BPF/CAP_SYS_ADMIN and a kernel with task iterators"]
    fn resolves_preexisting_process_in_nested_pid_namespace() {
        const CHILD_ENV: &str = "ACTPLANE_TEST_TARGET_IDENTITY_PIDNS";
        if std::env::var_os(CHILD_ENV).is_none() {
            let test_binary = std::env::current_exe().expect("current test binary");
            let status = std::process::Command::new("unshare")
                .args(["--pid", "--fork", "--mount-proc"])
                .arg(test_binary)
                .args([
                    "target_identity::tests::resolves_preexisting_process_in_nested_pid_namespace",
                    "--exact",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD_ENV, "1")
                .status()
                .expect("spawn PID-namespaced identity test");
            assert!(status.success(), "nested target identity probe failed");
            return;
        }

        let mut target = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn pre-existing target");
        let local_pid = target.id() as i32;
        let start_time = target_start_time(local_pid);
        let identity = resolve_process_identity(local_pid, start_time)
            .expect("resolve existing target through live task iterator");
        let kernel_pid = identity.kernel_pid;
        assert_ne!(kernel_pid, local_pid, "outer and local PIDs must differ");
        assert_eq!(
            fs::metadata(format!("/proc/{kernel_pid}/ns/pid"))
                .expect_err("kernel PID must be hidden by container procfs")
                .kind(),
            io::ErrorKind::NotFound
        );
        target.kill().expect("kill target");
        target.wait().expect("reap target");
    }

    #[test]
    #[ignore = "requires root/CAP_BPF/CAP_SYS_ADMIN, pre-5.15 tracepoints and --test-threads=1"]
    fn nested_crash_left_tracepoint_holder_requires_confirmed_cleanup() {
        const NESTED_ENV: &str = "ACTPLANE_TEST_HOLDER_RECOVERY_PIDNS";
        const CRASH_ENV: &str = "ACTPLANE_TEST_HOLDER_RECOVERY_CRASH";
        const TEST_NAME: &str =
            "target_identity::tests::nested_crash_left_tracepoint_holder_requires_confirmed_cleanup";

        if std::env::var_os(NESTED_ENV).is_none() {
            let pins = std::path::PathBuf::from("/sys/fs/bpf").join(format!(
                "pidns-holder-recovery-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock")
                    .as_nanos()
            ));
            struct PinCleanup(std::path::PathBuf);
            impl Drop for PinCleanup {
                fn drop(&mut self) {
                    if self.0.exists() {
                        std::fs::remove_dir_all(&self.0).expect("remove private pins");
                    }
                }
            }
            let _cleanup = PinCleanup(pins.clone());
            // SAFETY: ignored live suites are explicitly run serially in their own test process.
            unsafe {
                std::env::set_var("ACTPLANE_BPF_PIN_ROOT", &pins);
                std::env::set_var("ACTPLANE_PINNED_PROFILE", "agent-file-guard");
            }
            let status = std::process::Command::new("unshare")
                .args(["--pid", "--fork"])
                .arg(std::env::current_exe().expect("current test binary"))
                .args([
                    TEST_NAME,
                    "--exact",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(NESTED_ENV, "1")
                .status()
                .expect("spawn nested tracepoint holder recovery test");
            assert!(status.success(), "nested holder recovery test failed");
            return;
        }

        let paths = crate::PinnedEnginePaths::from_env();
        if std::env::var_os(CRASH_ENV).is_some() {
            let engine = crate::PinnedEngine::open_or_install_singleton()
                .expect("install crash-left tracepoint holder");
            assert!(
                engine._tracepoint_holder.is_some(),
                "this regression requires process-held pre-5.15 tracepoints"
            );
            // Simulate a crash: process exit bypasses TracepointMarker::drop.
            std::process::exit(0);
        }

        let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                TEST_NAME,
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CRASH_ENV, "1")
            .status()
            .expect("spawn crashing tracepoint holder");
        assert!(status.success(), "holder installation process failed");
        let markers: Vec<_> = std::fs::read_dir(paths.maps_dir())
            .expect("list holder markers")
            .map(|entry| entry.expect("holder marker entry").path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(crate::TRACEPOINT_HOLDER_PREFIX))
            })
            .collect();
        assert_eq!(markers.len(), 1, "crash must leave one holder marker");

        let error = crate::PinnedEngine::open_or_install_singleton()
            .expect_err("an unverifiable holder must block restart");
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        let message = error.to_string();
        for expected in [
            "not visible from this PID namespace",
            "left the pin root",
            "manually remove only this pin root",
            paths.root.to_str().expect("ASCII pin root"),
        ] {
            assert!(
                message.contains(expected),
                "missing recovery detail: {message}"
            );
        }
        assert!(paths.root.exists());
        assert!(markers[0].exists(), "failed restart must retain pins");

        crate::remove_stale_pin_root(&paths).expect("confirmed manual cleanup");
        assert!(!markers[0].exists());
        let engine = crate::PinnedEngine::open_or_install_singleton()
            .expect("restart after confirmed cleanup");
        assert!(engine._tracepoint_holder.is_some());
    }

    #[test]
    #[ignore = "requires root/CAP_BPF/CAP_SYS_ADMIN and time/PID/task namespaces"]
    fn time_namespace_identity_is_fail_closed_and_offset_aware() {
        const MODE_ENV: &str = "ACTPLANE_TEST_TARGET_IDENTITY_TIME_NS";
        const TEST_NAME: &str =
            "target_identity::tests::time_namespace_identity_is_fail_closed_and_offset_aware";
        const OFFSET_NS: i128 = 5_000_000_000;
        const ENTER_OFFSET_NAMESPACE: &str = r#"
import ctypes, os, sys
libc = ctypes.CDLL(None, use_errno=True)
if libc.unshare(0x80) != 0:
    error = ctypes.get_errno()
    raise OSError(error, os.strerror(error))
with open('/proc/self/timens_offsets', 'w') as offsets:
    offsets.write('boottime 5 0\n')
os.execvp('unshare', [
    'unshare', '--pid', '--fork', '--mount-proc',
    sys.argv[1], sys.argv[2], '--exact', '--ignored', '--nocapture', '--test-threads=1',
])
"#;

        let mode = std::env::var(MODE_ENV).ok();
        if mode.is_none() {
            let test_binary = std::env::current_exe().expect("test binary");
            let status = std::process::Command::new(&test_binary)
                .args([
                    TEST_NAME,
                    "--exact",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(MODE_ENV, "mixed")
                .status()
                .expect("spawn isolated mixed time namespace test");
            assert!(status.success(), "mixed time namespace guard failed");
            let status = std::process::Command::new("python3")
                .args(["-c", ENTER_OFFSET_NAMESPACE])
                .arg(&test_binary)
                .arg(TEST_NAME)
                .env(MODE_ENV, "offset")
                .status()
                .expect("spawn offset time/PID namespace test");
            assert!(
                status.success(),
                "offset time namespace identity probe failed"
            );
            return;
        }

        if mode.as_deref() == Some("mixed") {
            // SAFETY: this isolated thread intentionally changes only its child time namespace.
            let result = unsafe { libc::unshare(libc::CLONE_NEWTIME) };
            assert_eq!(
                result,
                0,
                "unshare time namespace: {}",
                io::Error::last_os_error()
            );
            let error = procfs_start_time_from_boottime(10_000_000, 0)
                .expect_err("mixed current and child time namespaces must fail closed");
            assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
            return;
        }

        assert_eq!(mode.as_deref(), Some("offset"));
        assert_eq!(
            read_boottime_offset().expect("stable time namespace").value,
            OFFSET_NS
        );
        let mut target = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn pre-existing offset-namespace target");
        let local_pid = target.id() as i32;
        let start_time = target_start_time(local_pid);
        let identity = resolve_process_identity(local_pid, start_time)
            .expect("resolve target through its time namespace offset");
        let kernel_pid = identity.kernel_pid;
        assert_ne!(kernel_pid, local_pid, "outer and local PIDs must differ");
        let records = read_records().expect("read target start_boottime");
        let raw_start = records
            .iter()
            .find(|record| record.kernel_pid == kernel_pid)
            .expect("resolved target record")
            .start_boottime;
        assert_eq!(
            procfs_start_time_from_boottime(raw_start, identity.procfs_start_time_bias)
                .expect("convert target start_boottime"),
            start_time
        );
        target.kill().expect("kill target");
        target.wait().expect("reap target");
    }
}
