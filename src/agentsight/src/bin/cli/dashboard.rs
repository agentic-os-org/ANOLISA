//! Dashboard subcommand — display dashboard URL, auth status, and ECS access guide

use std::net::TcpStream;
use std::time::Duration;

use agentsight::ecs_metadata::{EcsMetadata, probe_ecs_metadata};
use agentsight::server::auth::DashboardAuth;
use structopt::StructOpt;

use super::{DEFAULT_CONFIG_PATH, load_server_auth_config};

/// Display the AgentSight dashboard URL and ECS access guide
#[derive(Debug, StructOpt, Clone)]
pub struct DashboardCommand {
    /// Custom database path (used to locate the token file)
    #[structopt(long)]
    pub db: Option<String>,

    /// Host the server is bound to (use a specific IP/hostname to override the Network URL)
    #[structopt(long, default_value = "0.0.0.0")]
    pub host: String,

    /// Port the server is listening on
    #[structopt(long, default_value = "7396")]
    pub port: u16,

    /// Do not attempt to open a browser
    #[structopt(long)]
    pub no_open: bool,

    /// Skip ECS security group guide output
    #[structopt(long)]
    pub skip_sg_guide: bool,

    /// Path to JSON configuration file
    #[structopt(long, default_value = DEFAULT_CONFIG_PATH)]
    pub config: String,
}

impl DashboardCommand {
    pub fn execute(&self) {
        // Check if the server is running
        if !check_server_running(self.port) {
            eprintln!("AgentSight 服务未启动。请先运行 `agentsight serve`。");
            std::process::exit(1);
        }

        let output = self.build_output();

        println!();
        for line in &output.lines {
            println!("{line}");
        }

        // ECS security group guide
        if let Some(ref msg) = output.sg_message {
            println!("{msg}");
        }

        // Try to open browser
        if !self.no_open {
            try_open_browser(&output.display_url);
        }
    }

    /// Compute all display information without performing I/O.
    fn build_output(&self) -> DashboardOutput {
        let local_url = format!("http://127.0.0.1:{}", self.port);

        let ecs = if self.skip_sg_guide {
            None
        } else {
            probe_ecs_metadata()
        };

        let storage_base = self
            .db
            .as_ref()
            .map(std::path::PathBuf::from)
            .and_then(|p| p.parent().map(|pp| pp.to_path_buf()))
            .unwrap_or_else(|| {
                agentsight::storage::sqlite::GenAISqliteStore::default_path()
                    .parent()
                    .unwrap_or(std::path::Path::new("/var/log/sysak/.agentsight"))
                    .to_path_buf()
            });

        let auth_config = load_server_auth_config(&self.config);
        let auth = DashboardAuth::init(&auth_config, &storage_base);
        let token = auth.read_token_from_file();

        // When --host is a specific address (not wildcard), use it as override
        let host_override = if self.host != "0.0.0.0" && self.host != "::" {
            Some(self.host.clone())
        } else {
            None
        };

        // Resolve the primary display URL (ECS public IP > --host > LAN IP > localhost)
        let display_url = resolve_display_url(&ecs, &host_override, &local_url, self.port);

        // Build output lines
        let mut lines = Vec::new();
        lines.push("AgentSight 仪表盘状态".to_string());
        lines.push("=====================".to_string());
        lines.push(String::new());

        if auth.enabled {
            lines.push("  认证:    已启用".to_string());
        } else {
            lines.push("  认证:    已关闭".to_string());
        }

        // Localhost (loopback bypasses auth)
        lines.push(format!(
            "  本机:    {local_url}{}",
            if auth.enabled { " (无需认证)" } else { "" }
        ));

        // LAN (private) IP
        let lan_ip = host_override
            .as_deref()
            .map(str::to_string)
            .or_else(|| local_addresses().into_iter().next());
        if let Some(ref ip) = lan_ip {
            lines.push(format_url("  局域网:", ip, self.port, token.as_deref()));
        }

        // Public IP (ECS metadata > curl detection > hint)
        let public_ip = ecs
            .as_ref()
            .and_then(|m| m.public_ip().map(|s| s.to_string()))
            .or_else(|| {
                if host_override.is_none() {
                    public_address()
                } else {
                    None
                }
            });
        match public_ip {
            Some(ref ip) => {
                lines.push(format_url("  公网:", ip, self.port, token.as_deref()));
            }
            None => {
                lines.push("  公网:    (无法检测 — 请使用 --host <公网IP> 指定)".to_string());
            }
        }

        // Tip for --host usage
        if auth.enabled && host_override.is_none() {
            lines.push(String::new());
            lines.push("  提示: 使用 --host <IP> 可覆盖显示的网络地址,".to_string());
            lines.push("        例如: agentsight dashboard --host <你的公网IP>".to_string());
        }

        let sg_message = build_sg_message(&ecs, self.skip_sg_guide, self.port);

        DashboardOutput {
            display_url,
            lines,
            sg_message,
        }
    }
}

/// Pre-computed display data for the dashboard command.
#[derive(Debug)]
struct DashboardOutput {
    display_url: String,
    lines: Vec<String>,
    sg_message: Option<String>,
}

/// Determine the display URL: ECS public IP > --host > LAN IP > localhost.
fn resolve_display_url(
    ecs: &Option<EcsMetadata>,
    host_override: &Option<String>,
    local_url: &str,
    port: u16,
) -> String {
    if let Some(meta) = ecs
        && let Some(ip) = meta.public_ip()
    {
        return format!("http://{ip}:{port}");
    }
    if let Some(h) = host_override {
        return format!("http://{h}:{port}");
    }
    local_addresses()
        .into_iter()
        .next()
        .map(|ip| format!("http://{ip}:{port}"))
        .unwrap_or_else(|| local_url.to_string())
}

/// Build the security group guide message, if applicable.
fn build_sg_message(ecs: &Option<EcsMetadata>, skip: bool, port: u16) -> Option<String> {
    match ecs {
        Some(meta) => {
            let mut msg = String::new();
            msg.push_str(&format!(
                "远程打不开？请前往实例控制台配置安全组，放行 TCP {port}：\n"
            ));
            msg.push_str(&format!("  {}", meta.instance_url()));
            msg.push('\n');
            Some(msg)
        }
        None if !skip => Some(format!(
            "未检测到 ECS 环境，请手动确保防火墙/安全组已放行 {port} 端口。\n"
        )),
        None => None,
    }
}

/// Format a URL line with optional token.
fn format_url(label: &str, host: &str, port: u16, token: Option<&str>) -> String {
    match token {
        Some(t) => format!("{label:<8} http://{host}:{port}/?token={t}"),
        None => format!("{label:<8} http://{host}:{port}"),
    }
}

/// Detect the public IP address via external service.
///
/// Uses `curl` with a 3-second timeout to query a lightweight IP echo service.
/// Returns `None` if detection fails (no network, curl missing, timeout, etc.).
fn public_address() -> Option<String> {
    let output = std::process::Command::new("curl")
        .args(["-s", "-m", "3", "--retry", "1", "https://ifconfig.me"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let ip = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // Validate that the response looks like an IP or hostname (not an HTML error page)
    if ip.is_empty() || ip.len() > 64 || ip.contains('<') {
        return None;
    }
    Some(ip)
}

/// Get non-loopback local IP addresses for URL display.
fn local_addresses() -> Vec<String> {
    let Ok(output) = std::process::Command::new("ip")
        .args(["-4", "addr", "show"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("inet ")
                .and_then(|r| r.split('/').next())
        })
        .filter_map(|ip_str| ip_str.trim().parse::<std::net::Ipv4Addr>().ok())
        .filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
        .map(|ip| ip.to_string())
        .collect()
}

/// Quick TCP connect to check whether the server is listening.
fn check_server_running(port: u16) -> bool {
    TcpStream::connect_timeout(
        &std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port),
        Duration::from_millis(500),
    )
    .is_ok()
}

/// Try to open a URL in the default browser.
fn try_open_browser(url: &str) {
    let opener = find_executable("xdg-open");
    if let Some(bin) = opener {
        let _ = std::process::Command::new(bin)
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

/// Check whether an executable exists in `$PATH`.
fn find_executable(name: &str) -> Option<String> {
    let path_var = std::env::var("PATH").unwrap_or_default();
    path_var
        .split(':')
        .map(|dir| format!("{dir}/{name}"))
        .find(|full| std::path::Path::new(full).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn find_executable_returns_path_for_known_command() {
        // "ls" should exist on any Linux system
        let result = find_executable("ls");
        assert!(result.is_some(), "ls should be found in PATH");
        let path = result.unwrap();
        assert!(
            std::path::Path::new(&path).is_file(),
            "returned path should be a file: {path}"
        );
    }

    #[test]
    fn find_executable_returns_none_for_nonexistent_command() {
        let result = find_executable("__nonexistent_binary_xyz__");
        assert!(result.is_none(), "nonexistent command should return None");
    }

    #[test]
    fn local_addresses_returns_valid_ipv4_or_empty() {
        let addrs = local_addresses();
        for addr in &addrs {
            let parsed: Result<std::net::Ipv4Addr, _> = addr.parse();
            assert!(parsed.is_ok(), "each address should be valid IPv4: {addr}");
            let ip = parsed.unwrap();
            assert!(!ip.is_loopback(), "should exclude loopback: {ip}");
            assert!(!ip.is_unspecified(), "should exclude unspecified: {ip}");
        }
    }

    #[test]
    fn check_server_running_returns_false_when_no_listener() {
        // Pick a port that is almost certainly not in use
        let result = check_server_running(1);
        assert!(
            !result,
            "port 1 should not have a listener, so check should return false"
        );
    }

    #[test]
    fn check_server_running_returns_true_with_active_listener() {
        // Bind a TCP listener on a random port
        let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind");
        let port = listener.local_addr().unwrap().port();
        let result = check_server_running(port);
        assert!(result, "should detect the active listener on port {port}");
    }

    #[test]
    fn resolve_display_url_with_ecs_public_ip() {
        let ecs = Some(EcsMetadata {
            instance_id: "i-test".to_string(),
            region_id: "cn-hangzhou".to_string(),
            eip: "1.2.3.4".to_string(),
            public_ipv4: String::new(),
        });
        let url = resolve_display_url(&ecs, &None, "http://127.0.0.1:7396", 7396);
        assert_eq!(url, "http://1.2.3.4:7396");
    }

    #[test]
    fn resolve_display_url_with_host_override() {
        let url = resolve_display_url(
            &None,
            &Some("8.8.8.8".to_string()),
            "http://127.0.0.1:7396",
            7396,
        );
        assert_eq!(url, "http://8.8.8.8:7396");
    }

    #[test]
    fn resolve_display_url_falls_back_to_local_when_no_public_ip() {
        let ecs = Some(EcsMetadata {
            instance_id: "i-test".to_string(),
            region_id: "cn-hangzhou".to_string(),
            eip: String::new(),
            public_ipv4: String::new(),
        });
        let url = resolve_display_url(&ecs, &None, "http://127.0.0.1:7396", 7396);
        // Without public IP, falls through to local_addresses() or localhost fallback
        assert!(url.starts_with("http://"));
        assert!(url.ends_with(":7396"));
    }

    #[test]
    fn resolve_display_url_without_ecs() {
        let url = resolve_display_url(&None, &None, "http://127.0.0.1:7396", 7396);
        // Should use local_addresses or fall back to localhost
        assert!(url.starts_with("http://"));
        assert!(url.ends_with(":7396"));
    }

    #[test]
    fn build_sg_message_with_ecs_metadata() {
        let ecs = Some(EcsMetadata {
            instance_id: "i-abc123".to_string(),
            region_id: "cn-hangzhou".to_string(),
            eip: "1.2.3.4".to_string(),
            public_ipv4: String::new(),
        });
        let msg = build_sg_message(&ecs, false, 7396);
        let msg = msg.expect("should produce a message");
        assert!(msg.contains("7396"));
        assert!(msg.contains("i-abc123"));
        assert!(msg.contains("安全组"));
    }

    #[test]
    fn build_sg_message_none_when_skipped() {
        let msg = build_sg_message(&None, true, 7396);
        assert!(msg.is_none(), "skip=true should produce None");
    }

    #[test]
    fn build_sg_message_without_ecs() {
        let msg = build_sg_message(&None, false, 7396);
        let msg = msg.expect("should produce a message when not skipped");
        assert!(msg.contains("7396"));
        assert!(msg.contains("未检测到"));
    }

    #[test]
    fn format_url_with_token() {
        let line = format_url("  公网:", "1.2.3.4", 7396, Some("abc123"));
        assert!(line.contains("1.2.3.4"));
        assert!(line.contains("7396"));
        assert!(line.contains("abc123"));
        assert!(line.contains("公网"));
    }

    #[test]
    fn format_url_without_token() {
        let line = format_url("  局域网:", "10.0.0.1", 7396, None);
        assert!(line.contains("10.0.0.1"));
        assert!(line.contains("7396"));
        assert!(!line.contains("token"));
    }

    // ─── build_output tests ─────────────────────────────────────────────────

    /// Write a minimal agentsight config file and return its path.
    fn write_temp_config(auth_enabled: bool, suffix: &str) -> String {
        let dir =
            std::env::temp_dir().join(format!("agentsight_test_{}_{}", std::process::id(), suffix));
        std::fs::create_dir_all(&dir).ok();
        let config_path = dir.join("config.json");
        let content = serde_json::json!({
            "schema_version": agentsight::config::CURRENT_SCHEMA_VERSION,
            "server": {"auth": {"enabled": auth_enabled}},
        });
        std::fs::write(&config_path, content.to_string()).unwrap();
        config_path.to_string_lossy().to_string()
    }

    /// Return a unique temp storage directory for a given suffix.
    fn temp_storage_dir(suffix: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "agentsight_test_storage_{}_{}",
            std::process::id(),
            suffix
        ));
        std::fs::create_dir_all(&dir).ok();
        dir
    }

    #[test]
    fn build_output_with_auth_disabled() {
        let config = write_temp_config(false, "disabled");
        let storage = temp_storage_dir("disabled");
        let db_path = storage.join("test.db");
        std::fs::write(&db_path, b"").ok();

        let cmd = DashboardCommand {
            db: Some(db_path.to_string_lossy().to_string()),
            host: "0.0.0.0".to_string(),
            port: 7396,
            no_open: true,
            skip_sg_guide: true,
            config,
        };

        let output = cmd.build_output();

        // Should contain Chinese auth status
        assert!(output.lines.iter().any(|l| l.contains("已关闭")));
        // Should contain localhost URL
        assert!(output.lines.iter().any(|l| l.contains("127.0.0.1:7396")));
        // No token in URLs (auth disabled)
        assert!(!output.lines.iter().any(|l| l.contains("token=")));
        // No tip (auth disabled)
        assert!(!output.lines.iter().any(|l| l.contains("提示")));
        // SG message should be None (skip_sg_guide = true)
        assert!(output.sg_message.is_none());
    }

    #[test]
    fn build_output_with_auth_enabled() {
        let config = write_temp_config(true, "enabled");
        let storage = temp_storage_dir("enabled");
        let db_path = storage.join("test_auth.db");
        std::fs::write(&db_path, b"").ok();

        let cmd = DashboardCommand {
            db: Some(db_path.to_string_lossy().to_string()),
            host: "0.0.0.0".to_string(),
            port: 7396,
            no_open: true,
            skip_sg_guide: true,
            config,
        };

        let output = cmd.build_output();

        // Should contain Chinese auth status
        assert!(output.lines.iter().any(|l| l.contains("已启用")));
        // Should contain localhost URL with "无需认证"
        assert!(output.lines.iter().any(|l| l.contains("无需认证")));
        // Should contain tip
        assert!(output.lines.iter().any(|l| l.contains("提示")));
    }

    #[test]
    fn build_output_with_host_override() {
        let config = write_temp_config(true, "host");
        let storage = temp_storage_dir("host");
        let db_path = storage.join("test_host.db");
        std::fs::write(&db_path, b"").ok();

        let cmd = DashboardCommand {
            db: Some(db_path.to_string_lossy().to_string()),
            host: "8.8.8.8".to_string(),
            port: 9999,
            no_open: true,
            skip_sg_guide: true,
            config,
        };

        let output = cmd.build_output();

        // Display URL should use the overridden host
        assert!(output.display_url.contains("8.8.8.8"));
        assert!(output.display_url.contains("9999"));
        // LAN line should show the overridden host
        assert!(output.lines.iter().any(|l| l.contains("8.8.8.8")));
        // No tip (host_override is set)
        assert!(!output.lines.iter().any(|l| l.contains("提示")));
    }

    #[test]
    fn build_output_without_skip_sg_guide() {
        let config = write_temp_config(false, "sg");
        let storage = temp_storage_dir("sg");
        let db_path = storage.join("test_sg.db");
        std::fs::write(&db_path, b"").ok();

        let cmd = DashboardCommand {
            db: Some(db_path.to_string_lossy().to_string()),
            host: "0.0.0.0".to_string(),
            port: 7396,
            no_open: true,
            skip_sg_guide: false,
            config,
        };

        let output = cmd.build_output();

        // SG message should be present (not skipped, likely no ECS in CI)
        assert!(output.sg_message.is_some());
        let msg = output.sg_message.unwrap();
        assert!(msg.contains("7396"));
    }

    // ─── public_address tests ───────────────────────────────────────────────

    #[test]
    fn public_address_returns_option_without_panicking() {
        // This makes a real network call; in CI it may succeed or fail.
        // The test just verifies the function doesn't panic and returns Option.
        let result = public_address();
        if let Some(ip) = &result {
            assert!(!ip.is_empty());
            assert!(!ip.contains('<'));
            assert!(ip.len() <= 64);
        }
    }

    // ─── try_open_browser tests ─────────────────────────────────────────────

    #[test]
    fn try_open_browser_does_not_panic() {
        // Just verify it doesn't panic even if no browser opener exists.
        try_open_browser("http://127.0.0.1:7396");
    }
}
