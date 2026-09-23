//! Trace subcommand - agent activity tracing
//!
//! Linux: full eBPF-based tracing (probes → parser → aggregator → storage), or
//! trajectory collection only when `--no-ebpf` is passed.
//! macOS: trajectory collection only (JSONL file scanning → ATIF → SQLite).

use structopt::StructOpt;

/// Trace subcommand
#[derive(Debug, StructOpt, Clone)]
pub struct TraceCommand {
    /// Enable verbose/debug output.
    #[structopt(short, long)]
    pub verbose: bool,

    /// Run as daemon in background (Linux only)
    #[cfg(target_os = "linux")]
    #[structopt(long)]
    pub daemon: bool,
    /// PID file path for daemon mode (Linux only)
    #[cfg(target_os = "linux")]
    #[structopt(long, default_value = "/tmp/agentsight.pid")]
    pub pid_file: String,

    /// Enable file watch probe (monitors .jsonl file opens from traced processes)
    #[cfg(target_os = "linux")]
    #[structopt(long)]
    pub enable_filewatch: bool,

    /// Path to JSON configuration file (Linux only)
    #[cfg(target_os = "linux")]
    #[structopt(short, long, default_value = "/etc/agentsight/config.json")]
    pub config: String,

    /// Skip eBPF probes and collect trajectories only (Linux only)
    ///
    /// Loading probes needs root or CAP_BPF/CAP_PERFMON, so an unprivileged run
    /// otherwise aborts at probe setup. Trajectory collection is pure user-space
    /// and keeps working. This flag implies trajectory collection regardless of
    /// `features.trajectory_collection.enabled`, since it is the only remaining
    /// data source in this mode.
    #[cfg(target_os = "linux")]
    #[structopt(long)]
    pub no_ebpf: bool,
}

impl TraceCommand {
    pub fn execute(&self) {
        #[cfg(target_os = "linux")]
        {
            self.execute_linux();
        }

        #[cfg(not(target_os = "linux"))]
        {
            agentsight::local::trace::run_local_trace(self.verbose);
        }
    }
}

// ─── Linux: eBPF + trajectory collector ──────────────────────────────────────

#[cfg(target_os = "linux")]
impl TraceCommand {
    fn execute_linux(&self) {
        if self.daemon {
            self.run_as_daemon();
            return;
        }

        self.run_selected_mode();
    }

    /// Dispatch to the eBPF pipeline or the probe-free trajectory collector.
    ///
    /// Shared by the foreground and daemon paths so `--no-ebpf` also applies
    /// when running in the background.
    fn run_selected_mode(&self) {
        if self.no_ebpf {
            self.run_trajectory_only();
        } else {
            self.run_tracing();
        }
    }

    /// Run as daemon process
    fn run_as_daemon(&self) {
        use daemonize::Daemonize;

        println!("Starting agentsight in daemon mode...");
        println!("PID file: {}", self.pid_file);

        let daemonize = Daemonize::new()
            .pid_file(&self.pid_file)
            .chown_pid_file(true)
            .working_directory("/tmp");

        match daemonize.start() {
            Ok(_) => {
                self.run_selected_mode();
            }
            Err(e) => {
                eprintln!("Failed to daemonize: {e}");
                std::process::exit(1);
            }
        }
    }

    /// Run the actual tracing logic using AgentSight
    fn run_tracing(&self) {
        use agentsight::{AgentSight, AgentsightConfig};

        // Build AgentSight config (empty target_pids means trace all processes).
        // Note: `traceEnabled=false` from agentsight.json does NOT stop the agent
        // — token consumption (LLM call) data must always be collected by default.
        // The toggle only affects the SLS upload layer (LogtailExporter): when
        // traceEnabled=false, conversation content fields (gen_ai.input.messages /
        // gen_ai.output.messages) are dropped from uploaded records, but token
        // metadata (model, provider, token counts, etc.) is still uploaded.
        let config = AgentsightConfig::new()
            .set_verbose(self.verbose)
            .set_enable_filewatch(self.enable_filewatch)
            .set_config_path(std::path::PathBuf::from(&self.config));

        // Create AgentSight (auto-attaches probes and starts polling)
        let mut sight = match AgentSight::new(config) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Failed to create AgentSight: {e}");
                std::process::exit(1);
            }
        };

        // Register Ctrl+C handler for graceful shutdown.
        // This ensures AgentSight is dropped normally, which triggers
        // Storage::drop → WAL checkpoint, flushing data to the main db file.
        let running = sight.running_flag();
        ctrlc::set_handler(move || {
            log::info!("Ctrl+C received, shutting down gracefully...");
            running.store(false, std::sync::atomic::Ordering::SeqCst);
        })
        .expect("Failed to set Ctrl+C handler");

        // Run event loop (blocks until running flag is set to false)
        match sight.run() {
            Ok(count) => {
                println!("\nReceived {count} events total");
                println!("Token usage data saved. Use 'agentsight token' to query.");
            }
            Err(e) => {
                eprintln!("Error during tracing: {e}");
                std::process::exit(1);
            }
        }
        // `sight` drops here → Storage::drop → checkpoint
    }

    /// Collect trajectories without loading eBPF probes.
    ///
    /// Mirrors the macOS trace path (agent JSONL sessions → ATIF →
    /// `trajectories.db`) so unprivileged Linux sandboxes still produce
    /// trajectory data. Blocks until Ctrl+C.
    fn run_trajectory_only(&self) {
        use agentsight::AgentsightConfig;
        use agentsight_trajectory_collector::{CollectorConfig, run_collector_loop};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let config_path = std::path::PathBuf::from(&self.config);
        let mut config = AgentsightConfig::new()
            .set_verbose(self.verbose)
            .set_config_path(config_path.clone());
        // Scan interval and directories still come from the config file.
        // `ensure_default_agents_config` only materialises the default file; an
        // unprivileged run cannot write /etc/agentsight, and the load result
        // below reports whether any config was picked up.
        let _ = agentsight::config::ensure_default_agents_config(&config_path);
        let load_result = config.load_from_file(&config_path);
        config.apply_verbose();
        if let Err(e) = load_result {
            log::warn!("Config {config_path:?} unavailable ({e}); using built-in defaults");
        }

        let Some(db_path) = Self::resolve_trajectory_db_path(config.storage.trajectory_path())
        else {
            eprintln!(
                "Failed to open a writable trajectories.db in either the shared \
                 directory or $HOME; pass a writable HOME or grant write access."
            );
            std::process::exit(1);
        };
        // Warn about the directory the trajectory db will actually live in
        // (the shared dir or the $HOME fallback), not the unused default.
        if let Some(dir) = db_path.parent() {
            agentsight::container::warn_if_data_dir_not_persistent(dir);
        }

        let collector_config = CollectorConfig {
            scan_interval_secs: config.features.trajectory_scan_interval_secs,
            scan_dirs: config
                .features
                .trajectory_scan_dirs
                .as_ref()
                .map(|dirs| dirs.iter().map(std::path::PathBuf::from).collect()),
            db_path: db_path.clone(),
            maintenance: agentsight_trajectory_collector::TrajectoryMaintenancePolicy {
                retention_days: config.storage.trajectories.retention_days,
                max_db_size_mb: config.storage.trajectories.max_db_size_mb,
            },
            maintenance_interval_secs: config.storage.trajectories.check_interval_secs,
        };

        // `run_collector_loop` treats the flag as "keep running", so Ctrl+C
        // clears it instead of setting it.
        let running = Arc::new(AtomicBool::new(true));
        let stop = Arc::clone(&running);
        // A missing handler only costs the graceful stop, so the collector still
        // runs; say so instead of aborting a working collection.
        if let Err(e) = ctrlc::set_handler(move || {
            stop.store(false, Ordering::SeqCst);
        }) {
            eprintln!("Could not install the Ctrl+C handler ({e}); stop with SIGTERM instead.");
        }

        // Printed rather than logged: the collector is the only output of this
        // mode, and the log level may suppress info records.
        println!("eBPF disabled (--no-ebpf): collecting trajectories only.");
        println!(
            "Database: {} (scan interval {}s)",
            db_path.display(),
            collector_config.scan_interval_secs
        );
        // `serve` derives trajectories.db from the --db directory, so a
        // non-default location has to be passed through explicitly.
        if let Some(dir) = db_path.parent() {
            println!(
                "View with: agentsight serve --db {}/genai_events.db",
                dir.display()
            );
        }
        println!("Press Ctrl+C to stop.");

        run_collector_loop(&collector_config, &running);
    }

    /// Pick a writable location for `trajectories.db`.
    ///
    /// Prefers the shared directory so `agentsight serve` finds the file without
    /// extra flags, then falls back to `$HOME/.local/share/agentsight` for
    /// unprivileged runs. Returns `None` when neither can be opened.
    fn resolve_trajectory_db_path(
        configured_path: std::path::PathBuf,
    ) -> Option<std::path::PathBuf> {
        // The `private` flag marks the home-directory fallback: it is the only
        // candidate whose parents may be traversable by other local users.
        let mut candidates = vec![(configured_path, false)];
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push((
                std::path::PathBuf::from(home)
                    .join(".local/share/agentsight")
                    .join("trajectories.db"),
                true,
            ));
        }

        candidates
            .into_iter()
            .find(|(path, private)| Self::prepare_trajectory_db(path, *private))
            .map(|(path, _)| path)
    }

    /// Report whether `path` can host the trajectory database, creating its
    /// parent directory and verifying the database opens.
    ///
    /// Opening also creates the schema, so the collector's own open cannot then
    /// fail on permissions. When `private` is set, the directory is restricted to
    /// `0700` and the database to `0600` before opening: trajectories embed whole
    /// conversations, which must not be readable by other local users. SQLite
    /// derives WAL/SHM modes from the main database, so pre-creating it at `0600`
    /// covers the sidecars as well.
    fn prepare_trajectory_db(path: &std::path::Path, private: bool) -> bool {
        use std::fs::{DirBuilder, OpenOptions, Permissions, set_permissions};
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

        let Some(parent) = path.parent() else {
            return false;
        };

        let created = if private {
            DirBuilder::new().recursive(true).mode(0o700).create(parent)
        } else {
            std::fs::create_dir_all(parent)
        };
        if let Err(e) = created {
            log::warn!("Trajectory DB directory {parent:?} unusable: {e}");
            return false;
        }

        if private {
            // `DirBuilder`'s mode only applies to directories it creates, so an
            // already-present directory from an earlier run needs tightening too.
            if let Err(e) = set_permissions(parent, Permissions::from_mode(0o700)) {
                log::warn!("Could not restrict {parent:?} to 0700: {e}");
                return false;
            }
            if let Err(e) = OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(path)
            {
                log::warn!("Trajectory DB {path:?} unusable: {e}");
                return false;
            }
            // The mode above is ignored for a file that already exists.
            if let Err(e) = set_permissions(path, Permissions::from_mode(0o600)) {
                log::warn!("Could not restrict {path:?} to 0600: {e}");
                return false;
            }
        }

        match agentsight_trajectory_collector::TrajectoryStore::new_with_path(path) {
            Ok(_) => true,
            Err(e) => {
                log::warn!("Trajectory DB {path:?} unusable: {e}");
                false
            }
        }
    }
}
