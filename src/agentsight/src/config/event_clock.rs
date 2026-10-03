//! Checked conversion between kernel monotonic and Unix event time.

#[cfg(target_os = "linux")]
use std::sync::atomic::Ordering;

/// Failure to map a kernel monotonic timestamp into Unix time.
#[derive(Debug, thiserror::Error)]
pub enum ClockConversionError {
    /// The kernel clock or namespace metadata could not be read.
    #[error("event clock read failed ({operation}): {source}")]
    Read {
        /// Clock or metadata operation that failed.
        operation: &'static str,
        /// Operating-system error.
        source: std::io::Error,
    },
    /// Namespace identity or offset metadata cannot be mapped safely.
    #[error("event clock namespace identity or monotonic offset is invalid")]
    Namespace,
    /// Scheduling delays prevented a sufficiently tight clock sample.
    #[error("event clock sampling exceeded the 5 ms bracket after three attempts")]
    UnstableSample,
    /// Clock arithmetic would produce an invalid Unix timestamp.
    #[error("event clock value is outside the unsigned nanosecond range")]
    Range,
    /// No valid calibration is available, or its wall-clock age exceeds five seconds.
    #[error("event clock calibration is unavailable or expired")]
    Unavailable,
    /// Kernel event time conversion is only supported on Linux.
    #[error("kernel event clock conversion requires Linux")]
    UnsupportedPlatform,
}

/// Map BPF CLOCK_MONOTONIC nanoseconds into Unix nanoseconds.
///
/// Container-visible `/proc/uptime` may have a different epoch and is not a
/// substitute for the BPF clock. A process-wide worker refreshes the calibration
/// once per second; conversion only reads the snapshot and current realtime.
/// The first standalone call initializes the worker; probe pollers initialize it
/// before registering callbacks. Failed refreshes invalidate the snapshot.
/// A wall-clock step may affect events until the next refresh and cannot be
/// reconstructed historically from a queued monotonic timestamp alone.
///
/// # Errors
/// Rejects unreadable clocks, inconsistent/malformed time-namespace metadata, unstable
/// samples, unavailable/expired calibration and arithmetic overflow.
pub fn ktime_to_unix_ns(ktime_ns: u64) -> Result<u64, ClockConversionError> {
    #[cfg(target_os = "linux")]
    {
        let sample = event_clock()?.snapshot()?;
        sample.convert(ktime_ns, read_event_clock(libc::CLOCK_REALTIME)?)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = ktime_ns;
        Err(ClockConversionError::UnsupportedPlatform)
    }
}

#[cfg(any(target_os = "linux", test))]
fn unix_ns_from_sample(
    ktime_ns: u64,
    realtime_ns: u64,
    monotonic_ns: u64,
) -> Result<u64, ClockConversionError> {
    u64::try_from(i128::from(realtime_ns) + i128::from(ktime_ns) - i128::from(monotonic_ns))
        .map_err(|_| ClockConversionError::Range)
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct ClockSample {
    realtime_ns: u64,
    monotonic_ns: u64,
}

#[cfg(target_os = "linux")]
impl ClockSample {
    fn convert(self, ktime_ns: u64, now_ns: u64) -> Result<u64, ClockConversionError> {
        // Realtime is shared across time namespaces. Clock steps outside this
        // window invalidate the sample even if the calibration worker stalls.
        if !matches!(now_ns.checked_sub(self.realtime_ns), Some(age) if age <= 5_000_000_000) {
            return Err(ClockConversionError::Unavailable);
        }
        unix_ns_from_sample(ktime_ns, self.realtime_ns, self.monotonic_ns)
    }
}

#[cfg(target_os = "linux")]
#[derive(Default)]
struct ClockCache(std::sync::RwLock<Option<ClockSample>>);

#[cfg(target_os = "linux")]
impl ClockCache {
    fn snapshot(&self) -> Result<ClockSample, ClockConversionError> {
        self.0
            .read()
            .map_err(|_| ClockConversionError::Unavailable)?
            .ok_or(ClockConversionError::Unavailable)
    }

    fn refresh(
        &self,
        sample: impl FnOnce() -> Result<(u64, u64), ClockConversionError>,
    ) -> Result<(), ClockConversionError> {
        // Never hold the lock across procfs I/O or clock sampling.
        let result = sample();
        let mut cached = self
            .0
            .write()
            .map_err(|_| ClockConversionError::Unavailable)?;
        *cached = None;
        let (realtime_ns, monotonic_ns) = result?;
        *cached = Some(ClockSample {
            realtime_ns,
            monotonic_ns,
        });
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn event_clock() -> Result<&'static std::sync::Arc<ClockCache>, ClockConversionError> {
    use std::sync::{Arc, OnceLock};
    static CLOCK: OnceLock<Result<Arc<ClockCache>, std::io::Error>> = OnceLock::new();
    CLOCK
        .get_or_init(|| {
            let cache = Arc::new(ClockCache::default());
            if let Err(error) = cache.refresh(sample_event_clock) {
                report_clock_error("calibration", &error);
            }
            let worker = Arc::clone(&cache);
            // One worker lasts for the process, independent of poller restarts.
            std::thread::Builder::new()
                .name("event-clock".into())
                .spawn(move || {
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(1));
                        if let Err(error) = worker.refresh(sample_event_clock) {
                            report_clock_error("calibration", &error);
                        }
                    }
                })?;
            Ok(cache)
        })
        .as_ref()
        .map_err(|source| ClockConversionError::Read {
            operation: "spawn event clock worker",
            source: std::io::Error::new(source.kind(), source.to_string()),
        })
}

#[cfg(target_os = "linux")]
pub(crate) fn initialize_event_clock() -> Result<(), ClockConversionError> {
    event_clock().map(|_| ())
}

#[cfg(target_os = "linux")]
fn monotonic_namespace_offset(content: &str) -> Result<i128, ClockConversionError> {
    let mut offset = None;
    for line in content.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() != Some("monotonic") {
            continue;
        }
        let seconds = fields.next().and_then(|s| s.parse::<i64>().ok());
        let nanos = fields.next().and_then(|s| s.parse::<u32>().ok());
        match (seconds, nanos) {
            (Some(seconds), Some(nanos))
                if nanos < 1_000_000_000 && fields.next().is_none() && offset.is_none() =>
            {
                offset = Some(i128::from(seconds) * 1_000_000_000 + i128::from(nanos));
            }
            _ => return Err(ClockConversionError::Namespace),
        }
    }
    offset.ok_or(ClockConversionError::Namespace)
}

#[cfg(target_os = "linux")]
fn host_monotonic_ns(local_ns: u64, offset_ns: i128) -> Result<u64, ClockConversionError> {
    u64::try_from(i128::from(local_ns) - offset_ns).map_err(|_| ClockConversionError::Range)
}

#[cfg(target_os = "linux")]
fn read_event_clock(clock: libc::clockid_t) -> Result<u64, ClockConversionError> {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: value is a writable timespec and clock_gettime retains no pointer.
    if unsafe { libc::clock_gettime(clock, &mut value) } != 0 {
        return Err(ClockConversionError::Read {
            operation: "clock_gettime",
            source: std::io::Error::last_os_error(),
        });
    }
    if value.tv_sec < 0 || !(0..1_000_000_000).contains(&value.tv_nsec) {
        return Err(ClockConversionError::Range);
    }
    u64::try_from(i128::from(value.tv_sec) * 1_000_000_000 + i128::from(value.tv_nsec))
        .map_err(|_| ClockConversionError::Range)
}

#[cfg(target_os = "linux")]
fn sample_clock_pair(
    mut read: impl FnMut(libc::clockid_t) -> Result<u64, ClockConversionError>,
) -> Result<(u64, u64), ClockConversionError> {
    for _ in 0..3 {
        let before = read(libc::CLOCK_MONOTONIC)?;
        let realtime = read(libc::CLOCK_REALTIME)?;
        let after = read(libc::CLOCK_MONOTONIC)?;
        if let Some(width) = after
            .checked_sub(before)
            .filter(|width| *width <= 5_000_000)
        {
            return Ok((realtime, before + width / 2));
        }
    }
    Err(ClockConversionError::UnstableSample)
}

#[cfg(target_os = "linux")]
fn namespace_identity(path: &'static str) -> Result<Option<(u64, u64)>, ClockConversionError> {
    use std::os::unix::fs::MetadataExt;
    match std::fs::metadata(path) {
        Ok(meta) => Ok(Some((meta.dev(), meta.ino()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // CONFIG_TIME_NS=n has no ns/time entry. A missing procfs must still
            // fail: only accept absence when the namespace directory is readable.
            std::fs::read_dir("/proc/thread-self/ns").map_err(|source| {
                ClockConversionError::Read {
                    operation: path,
                    source,
                }
            })?;
            Ok(None)
        }
        Err(source) => Err(ClockConversionError::Read {
            operation: path,
            source,
        }),
    }
}

#[cfg(target_os = "linux")]
fn namespace_offset(
    current: Option<(u64, u64)>,
    leader: Option<(u64, u64)>,
    children: Option<(u64, u64)>,
    read_offsets: impl FnOnce() -> Result<String, ClockConversionError>,
) -> Result<i128, ClockConversionError> {
    // timens_offsets describes time_for_children, which may differ after unshare.
    // The process leader can also have a different namespace from this thread.
    if current != leader || current != children {
        return Err(ClockConversionError::Namespace);
    }
    if current.is_none() {
        return Ok(0);
    }
    monotonic_namespace_offset(&read_offsets()?)
}

#[cfg(target_os = "linux")]
fn sample_event_clock() -> Result<(u64, u64), ClockConversionError> {
    #[cfg(test)]
    tests::SAMPLE_CALLS.with(|calls| calls.set(calls.get() + 1));
    let current = namespace_identity("/proc/thread-self/ns/time")?;
    let offset = namespace_offset(
        current,
        namespace_identity("/proc/self/ns/time")?,
        namespace_identity("/proc/self/ns/time_for_children")?,
        || {
            std::fs::read_to_string("/proc/self/timens_offsets").map_err(|source| {
                ClockConversionError::Read {
                    operation: "read /proc/self/timens_offsets",
                    source,
                }
            })
        },
    )?;
    let (realtime, monotonic) = sample_clock_pair(read_event_clock)?;
    // Another thread can change the leader's child namespace during the read.
    for path in [
        "/proc/thread-self/ns/time",
        "/proc/self/ns/time",
        "/proc/self/ns/time_for_children",
    ] {
        if namespace_identity(path)? != current {
            return Err(ClockConversionError::Namespace);
        }
    }
    Ok((realtime, host_monotonic_ns(monotonic, offset)?))
}

#[cfg(target_os = "linux")]
pub(crate) fn report_clock_error(probe: &str, error: &ClockConversionError) {
    static REJECTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = REJECTED.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    // Exponential reporting keeps a broken environment from flooding the log.
    if count.is_power_of_two() {
        log::error!("event_clock_conversion_failed probe={probe} rejected={count}: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn cached_clock_refresh_failure_expiry_and_recovery() {
        let cache = ClockCache::default();
        assert!(matches!(
            cache.snapshot(),
            Err(ClockConversionError::Unavailable)
        ));
        cache
            .refresh(|| {
                assert!(cache.0.try_read().is_ok());
                Ok((10_000_000_000, 1_000))
            })
            .unwrap();
        let old = cache.snapshot().unwrap();
        assert_eq!(old.convert(900, 11_000_000_000).unwrap(), 9_999_999_900);
        assert!(matches!(
            old.convert(900, 15_000_000_001),
            Err(ClockConversionError::Unavailable)
        ));
        assert!(matches!(
            old.convert(900, 9_000_000_000),
            Err(ClockConversionError::Unavailable)
        ));
        assert!(matches!(
            old.convert(u64::MAX, 11_000_000_000),
            Err(ClockConversionError::Range)
        ));

        assert!(
            cache
                .refresh(|| Err(ClockConversionError::Namespace))
                .is_err()
        );
        assert!(matches!(
            cache.snapshot(),
            Err(ClockConversionError::Unavailable)
        ));
        // A successful refresh adopts a wall-clock step and restores conversion.
        cache.refresh(|| Ok((20_000_000_000, 2_000))).unwrap();
        assert_eq!(
            cache
                .snapshot()
                .unwrap()
                .convert(1_900, 20_000_000_001)
                .unwrap(),
            19_999_999_900
        );
    }

    #[cfg(target_os = "linux")]
    thread_local! {
        pub(super) static SAMPLE_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn event_burst_reuses_calibration_without_sampling() {
        initialize_event_clock().unwrap();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..100_000 {
                        assert!(ktime_to_unix_ns(1_000).is_ok());
                    }
                    SAMPLE_CALLS.with(|calls| assert_eq!(calls.get(), 0));
                });
            }
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn background_clock_refreshes_without_events() {
        let cache = event_clock().unwrap();
        let first = cache.snapshot().unwrap().realtime_ns;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        while cache.snapshot().unwrap().realtime_ns == first {
            assert!(
                std::time::Instant::now() < deadline,
                "clock worker did not refresh"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn test_ktime_to_unix_ns_nonzero() {
        assert_eq!(unix_ns_from_sample(100, 1_000, 150).unwrap(), 950);
    }

    #[test]
    fn test_ktime_to_unix_ns_zero() {
        assert_eq!(unix_ns_from_sample(0, 1_000, 150).unwrap(), 850);
    }

    #[test]
    fn event_clock_ignores_container_uptime() {
        let realtime = 2_000_000_000_000_000_000;
        let monotonic = 287 * 86_400_000_000_000;
        let container_uptime = 2 * 3_600_000_000_000;
        let event = monotonic - 200_000_000;
        let expected = realtime - 200_000_000;
        assert_eq!(
            unix_ns_from_sample(event, realtime, monotonic).unwrap(),
            expected
        );
        assert_ne!(realtime - container_uptime + event, expected);
        // A later calibration reflects a wall-clock adjustment, not cached state.
        assert_eq!(
            unix_ns_from_sample(event, realtime + 1_000, monotonic).unwrap(),
            expected + 1_000
        );
        assert!(matches!(
            unix_ns_from_sample(0, 1, 2),
            Err(ClockConversionError::Range)
        ));
        assert!(matches!(
            unix_ns_from_sample(u64::MAX, 1, 0),
            Err(ClockConversionError::Range)
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn namespace_offsets_restore_the_bpf_clock() {
        let host = 300_000_000_000_u64;
        for (text, offset) in [
            (
                "monotonic 172800 0\nboottime 604800 0",
                172_800_000_000_000_i128,
            ),
            ("monotonic -2 500000000", -1_500_000_000),
            ("monotonic 0 1", 1),
        ] {
            let parsed = monotonic_namespace_offset(text).unwrap();
            assert_eq!(parsed, offset);
            let local = u64::try_from(i128::from(host) + offset).unwrap();
            assert_eq!(host_monotonic_ns(local, parsed).unwrap(), host);
            assert_eq!(
                unix_ns_from_sample(
                    host - 100,
                    1_000_000_000_000,
                    host_monotonic_ns(local, parsed).unwrap()
                )
                .unwrap(),
                999_999_999_900,
            );
        }
        assert!(host_monotonic_ns(0, 1).is_err());
        assert!(host_monotonic_ns(u64::MAX, -1).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn namespace_metadata_must_match_the_sampling_thread() {
        let current = Some((1, 2));
        let other = Some((1, 3));
        assert_eq!(
            namespace_offset(None, None, None, || panic!(
                "no offsets without CONFIG_TIME_NS"
            ))
            .unwrap(),
            0
        );
        assert_eq!(
            namespace_offset(current, current, current, || Ok("monotonic 42 0".into())).unwrap(),
            42_000_000_000
        );
        for (leader, children) in [(other, current), (current, other), (None, current)] {
            assert!(matches!(
                namespace_offset(current, leader, children, || panic!("mismatched namespace")),
                Err(ClockConversionError::Namespace)
            ));
        }
        assert!(matches!(
            namespace_offset(current, current, current, || Err(
                ClockConversionError::Read {
                    operation: "test offsets",
                    source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
                }
            )),
            Err(ClockConversionError::Read { .. })
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn event_clock_namespace_and_sampling_errors() {
        assert!(monotonic_namespace_offset("monotonic 0 0\nboottime 0 0\n").unwrap() == 0);
        for invalid in [
            "",
            "monotonic 0 1000000000",
            "monotonic 0 -1",
            "monotonic bad 0",
            "monotonic 0",
            "monotonic 0 0\nmonotonic 0 0",
        ] {
            assert!(monotonic_namespace_offset(invalid).is_err());
        }
        let mut sample = [100, 1_000, 104].into_iter();
        assert_eq!(
            sample_clock_pair(|_| Ok(sample.next().unwrap())).unwrap(),
            (1_000, 102)
        );
        let mut calls = 0;
        assert!(matches!(
            sample_clock_pair(|_| {
                calls += 1;
                Ok(calls * 10_000_000)
            }),
            Err(ClockConversionError::UnstableSample)
        ));
        assert_eq!(calls, 9);
        assert!(matches!(
            read_event_clock(i32::MAX),
            Err(ClockConversionError::Read { .. })
        ));
    }
}
