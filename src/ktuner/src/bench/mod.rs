use anyhow::Result;
use std::time::Instant;

/// Removes the wrapped path when dropped, so a mid-run write/sync failure
/// cannot leak the bench temp file. Unlink-while-open is fine on Linux: the
/// kernel keeps the inode alive until the file handle closes.
struct RemoveOnDrop<'a>(&'a str);
impl Drop for RemoveOnDrop<'_> {
    fn drop(&mut self) {
        std::fs::remove_file(self.0).ok();
    }
}

#[derive(Debug, Clone)]
pub struct BenchResult {
    pub name: String,
    pub value: f64,
    pub unit: String,
}

impl BenchResult {
    pub fn summary(&self) -> String {
        format!("{:.2} {}", self.value, self.unit)
    }
}

/// True if `dir` lives on tmpfs/ramfs (i.e. RAM, not a real disk).
fn is_tmpfs(dir: &str) -> bool {
    let Ok(c) = std::ffi::CString::new(dir) else {
        return false;
    };
    unsafe {
        let mut buf: libc::statfs = std::mem::zeroed();
        if libc::statfs(c.as_ptr(), &mut buf) == 0 {
            let t = buf.f_type & 0xffff_ffff;
            return t == 0x0102_1994 /* TMPFS_MAGIC */ || t == 0x8584_58f6u32 as i64 /* RAMFS_MAGIC */;
        }
    }
    false
}

/// Pick a directory for IO benchmarks that is backed by a real disk. /tmp is
/// frequently tmpfs (RAM), which would make the IO benchmark measure memory
/// speed and silently invalidate tune's before/after disk comparison. Prefer
/// the working directory, then /var/tmp, and only fall back to /tmp.
fn bench_io_dir() -> String {
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.to_string_lossy().to_string());
    }
    candidates.push("/var/tmp".to_string());
    candidates.push("/tmp".to_string());
    for dir in &candidates {
        if std::path::Path::new(dir).is_dir() && !is_tmpfs(dir) {
            return dir.clone();
        }
    }
    "/tmp".to_string()
}

pub fn run_all_verbose() -> Result<Vec<BenchResult>> {
    run_all_inner(true)
}

type BenchFn = fn() -> Result<BenchResult>;

fn run_all_inner(verbose: bool) -> Result<Vec<BenchResult>> {
    use std::io::Write;

    let benches: &[(&str, BenchFn)] = &[
        ("syscall", bench_syscall_overhead),
        ("上下文切换", bench_context_switch),
        ("内存带宽", bench_mem_bandwidth),
        ("内存延迟", bench_mem_latency),
        ("IO 延迟", bench_io_latency),
        ("IO 吞吐", bench_io_throughput),
        ("网络延迟", bench_net_latency),
    ];

    let is_tty = unsafe { libc::isatty(libc::STDOUT_FILENO) } != 0;

    let mut results = Vec::with_capacity(benches.len());
    for (i, (label, bench_fn)) in benches.iter().enumerate() {
        if verbose && is_tty {
            print!(
                "\r    ⋯ [{}/{}] 测量 {}...{}",
                i + 1,
                benches.len(),
                label,
                " ".repeat(10)
            );
            std::io::stdout().flush().ok();
        }
        results.push(bench_fn()?);
    }
    if verbose && is_tty {
        print!("\r{}\r", " ".repeat(60));
        std::io::stdout().flush().ok();
    }

    Ok(results)
}

fn bench_syscall_overhead() -> Result<BenchResult> {
    let iterations = 1_000_000u64;
    let start = Instant::now();

    for _ in 0..iterations {
        unsafe { libc::getpid() };
    }

    let elapsed = start.elapsed();
    let ns_per_call = elapsed.as_nanos() as f64 / iterations as f64;

    Ok(BenchResult {
        name: "syscall overhead".to_string(),
        value: ns_per_call,
        unit: "ns/call".to_string(),
    })
}

fn bench_context_switch() -> Result<BenchResult> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    let iterations = 100_000u64;
    let (mut sock1, mut sock2) = UnixStream::pair()?;
    let buf = [0u8; 1];

    let child = std::thread::spawn(move || {
        let mut rbuf = [0u8; 1];
        for _ in 0..iterations {
            sock2.read_exact(&mut rbuf).ok();
            sock2.write_all(&buf).ok();
        }
    });

    let start = Instant::now();
    let mut rbuf = [0u8; 1];
    for _ in 0..iterations {
        sock1.write_all(&buf)?;
        sock1.read_exact(&mut rbuf)?;
    }
    let elapsed = start.elapsed();

    child.join().unwrap();

    Ok(context_switch_result(elapsed, iterations))
}

/// Label the unix-socket ping-pong cost. One iteration is a full round-trip
/// that wakes the peer twice (once for the request, once for the reply), so
/// the value is per RTT — like `bench_net_latency` — not per single switch.
fn context_switch_result(elapsed: std::time::Duration, iterations: u64) -> BenchResult {
    let us_per_rtt = elapsed.as_micros() as f64 / iterations as f64;
    BenchResult {
        name: "context switch".to_string(),
        value: us_per_rtt,
        unit: "μs/RTT".to_string(),
    }
}

fn bench_mem_bandwidth() -> Result<BenchResult> {
    let size = 64 * 1024 * 1024; // 64MB
    let mut buf: Vec<u8> = vec![0u8; size];
    let iterations = 10;

    // Warm up
    for b in buf.iter_mut() {
        *b = 1;
    }

    let start = Instant::now();
    for _ in 0..iterations {
        let mut sum: u64 = 0;
        for chunk in buf.chunks(64) {
            sum = sum.wrapping_add(chunk[0] as u64);
        }
        std::hint::black_box(sum);
    }
    let elapsed = start.elapsed();

    let total_bytes = size as f64 * iterations as f64;
    let gb_per_sec = total_bytes / elapsed.as_secs_f64() / 1_000_000_000.0;

    Ok(BenchResult {
        name: "memory bandwidth".to_string(),
        value: gb_per_sec,
        unit: "GB/s".to_string(),
    })
}

/// A Fisher-Yates shuffle of `0..size` from a fixed seed, so the pointer
/// chain below is random but reproducible.
fn shuffled_order(size: usize, seed: u64) -> Vec<usize> {
    let mut chain: Vec<usize> = (0..size).collect();
    let mut seed = seed;
    for i in (1..size).rev() {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let j = (seed as usize) % (i + 1);
        chain.swap(i, j);
    }
    chain
}

/// The index chain [`bench_mem_latency`] chases: the whole `chain` order as
/// one cycle, every entry written exactly once.
///
/// Walking the order and linking each node to its successor
/// (`indices[pos] = next; pos = next`, starting at 0) instead places node
/// `chain[i]` at slot `chain[i - 1]`, so the link into slot 0 is written by
/// whichever step follows the order's `0` — and the order's last node is never
/// a write position at all. A `0` anywhere but the last slot therefore splits
/// the permutation into the cycle reachable from slot 0 and the tail behind it,
/// and the walk reports ns per access over that fragment: at the production
/// seed the `0` lands at slot 7224, so 7224 of the 4194304 entries are never
/// touched while the benchmark still reports the latency of a 32 MB structure.
fn pointer_chain(chain: &[usize]) -> Vec<usize> {
    let mut indices = vec![0usize; chain.len()];
    let Some(last) = chain.len().checked_sub(1) else {
        return indices;
    };
    for (i, &node) in chain.iter().enumerate() {
        indices[node] = chain[if i == last { 0 } else { i + 1 }];
    }
    indices
}

fn bench_mem_latency() -> Result<BenchResult> {
    let size = 4 * 1024 * 1024; // 4M entries = 32MB on 64-bit

    // Build linked list via index chain
    let indices = pointer_chain(&shuffled_order(size, 42));

    // Chase pointers
    let iterations = 2_000_000;
    let start = Instant::now();
    let mut idx = 0usize;
    for _ in 0..iterations {
        idx = indices[idx];
    }
    std::hint::black_box(idx);
    let elapsed = start.elapsed();

    let ns_per_access = elapsed.as_nanos() as f64 / iterations as f64;

    Ok(BenchResult {
        name: "memory latency".to_string(),
        value: ns_per_access,
        unit: "ns/access".to_string(),
    })
}

/// Scratch file path for an IO benchmark, unique to this process.
///
/// The cleanup guard is already in place; what the name has to add is that two
/// concurrent `ktuner bench` runs do not share a path. With a fixed name they
/// open the same file with `truncate(true)` and both report throughput numbers
/// measured while the other process was rewriting the file underneath them.
/// `bench_io_dir` prefers the current working directory, so the collision is
/// between two shells in the same project rather than an exotic setup.
fn bench_io_path(dir: &str, name: &str) -> String {
    format!("{dir}/.{name}.{}", std::process::id())
}

fn bench_io_latency() -> Result<BenchResult> {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let path = bench_io_path(&bench_io_dir(), "ktuner_io_bench");
    let path = path.as_str();
    let iterations = 1000u64;

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(libc::O_SYNC)
        .open(path)?;

    let data = [0u8; 4096];

    let _cleanup = RemoveOnDrop(path);

    // Warm up
    file.write_all(&data)?;

    let start = Instant::now();
    for _ in 0..iterations {
        file.write_all(&data)?;
    }
    let elapsed = start.elapsed();

    drop(_cleanup);

    let us_per_write = elapsed.as_micros() as f64 / iterations as f64;

    Ok(BenchResult {
        name: "IO latency (sync)".to_string(),
        value: us_per_write,
        unit: "μs/write".to_string(),
    })
}

fn bench_io_throughput() -> Result<BenchResult> {
    use std::fs::OpenOptions;
    use std::io::Write;

    let path = bench_io_path(&bench_io_dir(), "ktuner_io_tput_bench");
    let path = path.as_str();
    let block_size = 1024 * 1024; // 1MB blocks
    let total_size = 128 * 1024 * 1024; // 128MB total
    let blocks = total_size / block_size;

    let data = vec![0u8; block_size];

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;

    let _cleanup = RemoveOnDrop(path);

    let start = Instant::now();
    for _ in 0..blocks {
        file.write_all(&data)?;
    }
    file.sync_all()?;
    let elapsed = start.elapsed();

    drop(_cleanup);

    Ok(io_throughput_result(total_size as u64, elapsed))
}

/// Label the sequential-write throughput. The math divides bytes by
/// 1024*1024, so the unit is MiB/s, not MB/s.
fn io_throughput_result(total_bytes: u64, elapsed: std::time::Duration) -> BenchResult {
    let mib_per_sec = total_bytes as f64 / elapsed.as_secs_f64() / (1024.0 * 1024.0);
    BenchResult {
        name: "IO throughput (seq)".to_string(),
        value: mib_per_sec,
        unit: "MiB/s".to_string(),
    }
}

fn bench_net_latency() -> Result<BenchResult> {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    let iterations = 50_000u64;

    let child = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 1];
        for _ in 0..iterations {
            stream.read_exact(&mut buf).ok();
            stream.write_all(&buf).ok();
        }
    });

    let mut stream = TcpStream::connect(addr)?;
    stream.set_nodelay(true)?;
    let buf = [0u8; 1];
    let mut rbuf = [0u8; 1];

    // Warm up
    for _ in 0..100 {
        stream.write_all(&buf)?;
        stream.read_exact(&mut rbuf)?;
    }

    let measure_iterations = iterations - 100;
    let start = Instant::now();
    for _ in 0..measure_iterations {
        stream.write_all(&buf)?;
        stream.read_exact(&mut rbuf)?;
    }
    let elapsed = start.elapsed();

    child.join().unwrap();

    let us_per_rtt = elapsed.as_micros() as f64 / measure_iterations as f64;

    Ok(BenchResult {
        name: "net latency (TCP)".to_string(),
        value: us_per_rtt,
        unit: "μs/RTT".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_on_drop_guard_removes_the_file() {
        let path =
            std::env::temp_dir().join(format!("ktuner-bench-guard-{}.tmp", std::process::id()));
        std::fs::write(&path, b"x").expect("write guard test file");
        {
            let _guard = RemoveOnDrop(path.to_str().expect("utf-8 temp path"));
            assert!(path.exists(), "file must exist while the guard is live");
        }
        assert!(!path.exists(), "guard must remove the file on drop");
    }

    #[test]
    fn the_pointer_chain_reaches_every_entry() {
        // One cycle through the whole permutation: `bench_mem_latency` reports
        // ns per access over the chain it walks, so a node the walk cannot
        // reach shortens the measured footprint without changing the count.
        let size = 16;
        let order = shuffled_order(size, 42);
        let zero_at = order.iter().position(|&node| node == 0).unwrap();
        assert!(
            zero_at != size - 1,
            "the production seed must place the zero mid-order, or this \
             fixture cannot exercise the split: {order:?}"
        );

        let indices = pointer_chain(&order);
        let mut seen = vec![false; size];
        let mut idx = 0usize;
        for _ in 0..size {
            assert!(!seen[idx], "the chain revisits entry {idx}: {order:?}");
            seen[idx] = true;
            idx = indices[idx];
        }
        assert_eq!(idx, 0, "the cycle must close on its start: {order:?}");
        assert!(
            seen.iter().all(|&visited| visited),
            "every entry must be reachable from 0: {seen:?} order {order:?}"
        );
    }

    #[test]
    fn the_pointer_chain_covers_a_single_entry() {
        // `size - 1` would underflow on the empty case's guard, so the one-node
        // ring is pinned separately: the walk must stay on 0 rather than read
        // an out-of-range slot.
        assert_eq!(pointer_chain(&[0]), vec![0]);
        assert!(pointer_chain(&[]).is_empty());
    }

    #[test]
    fn the_pointer_chain_reaches_every_entry_at_every_zero_position() {
        // The split depends on where the shuffle happened to place the 0, so
        // pin the positions rather than one seed's accident: start, middle and
        // end of the order.
        for zero_at in 0..6usize {
            let mut order: Vec<usize> = (1..=6).collect();
            order.insert(zero_at, 0);
            let indices = pointer_chain(&order);
            let mut seen = vec![false; order.len()];
            let mut idx = 0usize;
            for _ in 0..order.len() {
                assert!(
                    !seen[idx],
                    "zero at {zero_at} revisits entry {idx}: {order:?}"
                );
                seen[idx] = true;
                idx = indices[idx];
            }
            assert_eq!(idx, 0, "zero at {zero_at} does not close: {order:?}");
            assert!(
                seen.iter().all(|&visited| visited),
                "zero at {zero_at} leaves entries unreachable: {seen:?}"
            );
        }
    }

    #[test]
    fn io_throughput_is_labeled_mib_per_sec() {
        // 2 MiB written in 2 s is exactly 1.0 MiB/s; the divisor is
        // 1024*1024, so the label must be MiB/s, not MB/s.
        let r = io_throughput_result(2 * 1024 * 1024, std::time::Duration::from_secs(2));
        assert_eq!(r.unit, "MiB/s");
        assert_eq!(r.name, "IO throughput (seq)");
        assert!((r.value - 1.0).abs() < 1e-9);
    }

    #[test]
    fn context_switch_is_labeled_per_round_trip() {
        // One iteration is a full ping-pong round-trip (the peer wakes for
        // the request and again for the reply), so the unit is per RTT,
        // matching the μs/RTT convention of bench_net_latency.
        let r = context_switch_result(std::time::Duration::from_micros(200), 100);
        assert_eq!(r.unit, "μs/RTT");
        assert_eq!(r.name, "context switch");
        assert!((r.value - 2.0).abs() < 1e-9);
    }

    #[test]
    fn io_benchmark_paths_are_unique_per_process() {
        let dir = std::env::temp_dir();
        let dir = dir.to_string_lossy().to_string();

        let latency = bench_io_path(&dir, "ktuner_io_bench");
        let throughput = bench_io_path(&dir, "ktuner_io_tput_bench");
        assert_ne!(latency, throughput);

        // The process id is what keeps two concurrent runs apart, and a second
        // call from this process must still produce the same path so the guard
        // removes the file the benchmark actually wrote.
        assert!(latency.contains(&std::process::id().to_string()));
        assert_eq!(latency, bench_io_path(&dir, "ktuner_io_bench"));
        assert!(std::path::Path::new(&latency).parent() == Some(std::path::Path::new(&dir)));
    }
}
