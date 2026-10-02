//! SSL Sniffer Tool - Parse and print HTTP/SSE traffic from SSL connections
//!
//! Usage: sslsniff [OPTIONS]
//!
//! This tool captures SSL traffic and prints parsed HTTP requests, responses,
//! and SSE events in a human-readable format.

#[cfg(target_os = "linux")]
mod inner {
    use agentsight::config;
    use agentsight::parser::Parser;
    use agentsight::probes::sslsniff::SslSniff;
    use std::rc::Rc;
    use std::time::Duration;
    use structopt::StructOpt;

    #[derive(Debug, StructOpt)]
    #[structopt(
        name = "sslsniff",
        about = "Parse and print HTTP/SSE traffic from SSL connections"
    )]
    pub struct Command {
        /// Enable verbose/debug output
        #[structopt(short, long)]
        verbose: bool,

        /// Target PID
        #[structopt(short, long)]
        pid: i32,
    }

    pub fn run() {
        let opts = Command::from_args();
        config::set_verbose(opts.verbose);

        println!("Monitoring PID: {}", opts.pid);

        // Create SSL sniffer
        let mut sniffer = SslSniff::new().expect("Failed to create SSL sniffer");

        // Attach to target process
        sniffer
            .attach_process(opts.pid)
            .expect("Failed to attach SSL probe");

        // Start polling
        let _poller = sniffer.run().expect("Failed to start SSL poller");

        // Create unified parser
        let parser = Parser::new();

        println!("\n=== SSL Traffic Monitor ===\n");

        loop {
            if let Some(event) = sniffer.try_recv() {
                let result = parser.parse_ssl_event(Rc::new(event));
                for msg in result.messages {
                    println!("{msg:#?}");
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
    eprintln!("sslsniff is only available on Linux (requires eBPF)");
    std::process::exit(1);
}
