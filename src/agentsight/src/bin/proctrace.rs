//! Process Trace Tool - Monitor process execution and output
//!
//! Usage: proctrace [OPTIONS]
//!
//! This tool traces process execution (execve), stdout/stderr output, and exit events.

#[cfg(target_os = "linux")]
mod inner {
    use agentsight::config;
    use agentsight::parser::ProcTraceParser;
    use agentsight::probes::proctrace::ProcTrace;
    use std::time::Duration;
    use structopt::StructOpt;

    #[derive(Debug, StructOpt)]
    #[structopt(name = "proctrace", about = "Trace process execution and output")]
    pub struct Command {
        /// Enable verbose/debug output
        #[structopt(short, long)]
        verbose: bool,

        /// Target PID to trace (optional, trace all if not specified)
        #[structopt(short, long)]
        pid: Option<u32>,

        /// Filter by UID
        #[structopt(long)]
        uid: Option<u32>,
    }

    pub fn run() {
        let opts = Command::from_args();
        config::set_verbose(opts.verbose);

        // Create process tracer
        let target_pids: Vec<u32> = opts.pid.map(|p| vec![p]).unwrap_or_default();
        let mut tracer = ProcTrace::new_with_target(&target_pids, opts.uid)
            .expect("Failed to create process tracer");

        // Attach tracepoints
        tracer.attach().expect("Failed to attach tracepoints");

        // Start polling
        let _poller = tracer.run().expect("Failed to start process poller");

        println!("=== Process Tracer ===");
        if let Some(pid) = opts.pid {
            println!("Target PID: {pid}");
        } else {
            println!("Target: All processes");
        }
        if let Some(uid) = opts.uid {
            println!("UID filter: {uid}");
        }
        println!("\n");

        loop {
            if let Some(event) = tracer.try_recv() {
                if let Some(parsed) = ProcTraceParser::parse_variable(&event) {
                    println!("{parsed:#?}");
                }
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn main() {
    inner::run();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("proctrace is only available on Linux (requires eBPF)");
    std::process::exit(1);
}
