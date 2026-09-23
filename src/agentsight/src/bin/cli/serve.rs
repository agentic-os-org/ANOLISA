//! Serve subcommand — start the API server
//!
//! Linux: full eBPF server with SQLite, auth, health routes.
//! macOS: delegates to agentsight::local::server (trajectory viewer).

use structopt::StructOpt;

/// Start the AgentSight API server
#[derive(Debug, StructOpt, Clone)]
pub struct ServeCommand {
    /// Host to bind to
    #[structopt(long, default_value = "127.0.0.1")]
    pub host: String,

    /// Port to bind to
    #[structopt(long, default_value = "7396")]
    pub port: u16,

    /// Custom database path (Linux only)
    #[cfg(target_os = "linux")]
    #[structopt(long)]
    pub db: Option<String>,

    /// Path to JSON configuration file (Linux only)
    #[cfg(target_os = "linux")]
    #[structopt(long, default_value = super::DEFAULT_CONFIG_PATH)]
    pub config: String,

    /// Path to JSON configuration file.
    ///
    /// Optional here, unlike on Linux, and with no default: the Linux default
    /// lives under `/etc`, which an unprivileged macOS run cannot create, and
    /// warning about that on every start would be noise. Supply it to turn on
    /// features that need configuration, such as second-level labelling.
    #[cfg(not(target_os = "linux"))]
    #[structopt(long)]
    pub config: Option<String>,
}

impl ServeCommand {
    pub fn execute(&self) {
        let host = self.host.clone();
        let port = self.port;

        #[cfg(target_os = "linux")]
        {
            use agentsight::server::run_server;

            let mut server_config = super::load_server_config(&self.config);
            let db_path = self
                .db
                .as_ref()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| server_config.storage.genai_path());
            if self.db.is_some() {
                server_config.storage.base_path = db_path
                    .parent()
                    .filter(|path| !path.as_os_str().is_empty())
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .to_path_buf();
            }
            // Initialize logging before warning: standalone `serve` does not
            // register a logger, so a `log::warn!` before this point is lost.
            server_config.apply_verbose();
            let auth_config = server_config.server_auth;
            let storage_config = server_config.storage;
            let judge_enabled = server_config.features.reuse_llm_judge_enabled;

            if let Some(dir) = db_path.parent() {
                agentsight::container::warn_if_data_dir_not_persistent(dir);
            }

            actix_web::rt::System::new().block_on(async move {
                if let Err(e) = run_server(
                    &host,
                    port,
                    db_path,
                    auth_config,
                    storage_config,
                    judge_enabled,
                )
                .await
                {
                    eprintln!("Server error: {e}");
                    std::process::exit(1);
                }
            });
        }

        #[cfg(not(target_os = "linux"))]
        {
            let judge_enabled = self
                .config
                .as_deref()
                .map(|path| {
                    super::load_server_config(path)
                        .features
                        .reuse_llm_judge_enabled
                })
                .unwrap_or(false);

            actix_web::rt::System::new().block_on(async move {
                if let Err(e) =
                    agentsight::local::server::run_server(&host, port, judge_enabled).await
                {
                    eprintln!("Server error: {e}");
                    std::process::exit(1);
                }
            });
        }
    }
}
