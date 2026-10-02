use anyhow::Result;
use std::time::Instant;

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

    let us_per_switch = elapsed.as_micros() as f64 / iterations as f64;

    Ok(BenchResult {
        name: "context switch".to_string(),
        value: us_per_switch,
        unit: "μs/switch".to_string(),
    })
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

fn bench_mem_latency() -> Result<BenchResult> {
    let size = 4 * 1024 * 1024; // 4M entries = 32MB on 64-bit
    let mut chain: Vec<usize> = (0..size).collect();

    // Shuffle to create random pointer chain
    let mut seed = 42u64;
    for i in (1..size).rev() {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let j = (seed as usize) % (i + 1);
        chain.swap(i, j);
    }

    // Build linked list via index chain
    let mut indices = vec![0usize; size];
    let mut pos = 0;
    for &next in &chain {
        indices[pos] = next;
        pos = next;
    }

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

fn bench_io_latency() -> Result<BenchResult> {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let path = format!("{}/.ktuner_io_bench", bench_io_dir());
    let path = path.as_str();
    let iterations = 1000u64;

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(libc::O_SYNC)
        .open(path)?;

    let data = [0u8; 4096];

    // Warm up
    file.write_all(&data)?;

    let start = Instant::now();
    for _ in 0..iterations {
        file.write_all(&data)?;
    }
    let elapsed = start.elapsed();

    std::fs::remove_file(path).ok();

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

    let path = format!("{}/.ktuner_io_tput_bench", bench_io_dir());
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

    let start = Instant::now();
    for _ in 0..blocks {
        file.write_all(&data)?;
    }
    file.sync_all()?;
    let elapsed = start.elapsed();

    std::fs::remove_file(path).ok();

    let mb_per_sec = total_size as f64 / elapsed.as_secs_f64() / (1024.0 * 1024.0);

    Ok(BenchResult {
        name: "IO throughput (seq)".to_string(),
        value: mb_per_sec,
        unit: "MB/s".to_string(),
    })
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
