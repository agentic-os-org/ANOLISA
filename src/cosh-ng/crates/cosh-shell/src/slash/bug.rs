//! `/bug` builds a reviewable issue-form URL from allowlisted runtime facts.
//!
//! The command reads no logs, writes no files, and performs no network I/O.

use std::fs;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::runtime::prelude::*;
use crate::slash::panel::render_notice_panel;
use crate::slash::status::{provider_display, runtime_identity};

const DEFAULT_URL_TEMPLATE: &str = "https://github.com/agentic-os-org/ANOLISA/issues/new?template=bug_report.yml&component=cosh-ng&title={title}&diagnostics={info}";
const TITLE_LIMIT: usize = 120;

#[derive(Debug, Default, PartialEq, Eq)]
struct PlatformFacts {
    kernel: Option<String>,
    distro: Option<String>,
    install: Option<String>,
}

pub(super) fn render_bug_command<W: Write>(
    description: &str,
    adapter: &AdapterInstance,
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let facts = collect_platform_facts();
    let logs_directory = crate::runtime::logging::log_directory();
    let logs_exist = logs_directory.as_deref().is_some_and(Path::is_dir);
    let terminal_available = std::io::stdout().is_terminal();
    render_bug_command_with(
        description,
        adapter,
        state,
        &facts,
        logs_directory.as_deref(),
        logs_exist,
        terminal_available,
        open_issue_url,
        output,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_bug_command_with<W: Write, O: FnOnce(&str) -> bool>(
    description: &str,
    adapter: &AdapterInstance,
    state: &InlineState,
    facts: &PlatformFacts,
    logs_directory: Option<&Path>,
    logs_exist: bool,
    terminal_available: bool,
    opener: O,
    output: &mut W,
) -> std::io::Result<()> {
    let snapshot = environment_snapshot(adapter, state, facts);
    let url = build_issue_url(description, &snapshot);
    let i18n = state.i18n();
    let mut body = vec![
        i18n.t(MessageId::SlashBugLinkLine).to_string(),
        i18n.t(MessageId::SlashBugFormLine).to_string(),
    ];
    if logs_exist {
        if let Some(path) = logs_directory.and_then(Path::to_str) {
            body.push(i18n.format(MessageId::SlashBugLogsLine, &[("path", path)]));
        }
    }
    let opened = terminal_available && opener(&url);
    body.push(
        i18n.t(if opened {
            MessageId::SlashBugOpenedLine
        } else {
            MessageId::SlashBugCopyLine
        })
        .to_string(),
    );

    // No privacy-promise footer: any unqualified upload claim is wrong the
    // moment the browser fetches the pre-filled URL.
    render_notice_panel(output, i18n.t(MessageId::SlashBugTitle), body, None)?;
    // Printed outside the framed panel so terminal wrapping never splits the
    // URL with border glyphs, which would break copy-paste on servers.
    writeln!(output, "{url}")
}

fn environment_snapshot(
    adapter: &AdapterInstance,
    state: &InlineState,
    facts: &PlatformFacts,
) -> String {
    let identity = runtime_identity(adapter, state);
    let provider_session = adapter.committed_session_id();
    let session = provider_session
        .as_deref()
        .or(state.shell_session_id.as_deref());
    let mut lines = vec![
        format!("version: {}", env!("CARGO_PKG_VERSION")),
        format!("backend: {}", adapter.name()),
    ];
    push_fact(
        &mut lines,
        "provider",
        provider_display(&identity).as_deref(),
    );
    push_fact(&mut lines, "model", identity.model.as_deref());
    push_fact(&mut lines, "session", session);
    lines.push(format!(
        "os: {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    lines.push(format!(
        "modes: approval={}, analysis={}",
        state.approval_mode.label(),
        state.analysis_mode.label()
    ));
    push_fact(&mut lines, "kernel", facts.kernel.as_deref());
    push_fact(&mut lines, "distro", facts.distro.as_deref());
    if let Some(libc) = target_libc() {
        lines.push(format!("libc: {libc}"));
    }
    push_fact(&mut lines, "shell", state.shell_identity.as_deref());
    push_fact(&mut lines, "install", facts.install.as_deref());
    lines.push(format!("language: {}", state.language.as_config_value()));
    lines.join("\n")
}

fn push_fact(lines: &mut Vec<String>, label: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        lines.push(format!("{label}: {value}"));
    }
}

fn collect_platform_facts() -> PlatformFacts {
    PlatformFacts {
        kernel: kernel_release(),
        distro: fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|content| crate::diagnostics::health::parse_os_release_pretty_name(&content)),
        install: crate::upgrade::check::current_executable_rpm_package(),
    }
}

#[cfg(target_os = "linux")]
fn kernel_release() -> Option<String> {
    fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .and_then(trimmed_non_empty)
}

#[cfg(target_os = "macos")]
fn kernel_release() -> Option<String> {
    Command::new("uname")
        .arg("-r")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(trimmed_non_empty)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn kernel_release() -> Option<String> {
    None
}

fn trimmed_non_empty(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn target_libc() -> Option<&'static str> {
    if cfg!(target_env = "gnu") {
        Some("gnu")
    } else if cfg!(target_env = "musl") {
        Some("musl")
    } else {
        None
    }
}

fn build_issue_url(description: &str, snapshot: &str) -> String {
    let mut template = DEFAULT_URL_TEMPLATE.to_string();
    if description.trim().is_empty() {
        template = template.replace("&title={title}", "");
    }
    let title = description
        .trim()
        .chars()
        .take(TITLE_LIMIT)
        .collect::<String>();
    template
        .replacen("{title}", &percent_encode(&title), 1)
        .replacen("{info}", &percent_encode(snapshot), 1)
}

fn valid_http_url(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://"))
        && !url.chars().any(char::is_control)
}

fn percent_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[(byte >> 4) as usize]));
            encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
    encoded
}

fn open_issue_url(url: &str) -> bool {
    if !valid_http_url(url) {
        return false;
    }
    let Some(program) = browser_opener() else {
        return false;
    };
    match Command::new(program)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            // Reap the opener off the prompt thread so it never lingers as a zombie.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            true
        }
        Err(_) => false,
    }
}

// Without a graphical session `xdg-open` falls back to a text browser that
// would take over the shell's terminal, so headless servers only print the URL.
#[cfg(target_os = "linux")]
fn browser_opener() -> Option<&'static str> {
    (std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some())
        .then_some("xdg-open")
}

#[cfg(target_os = "macos")]
fn browser_opener() -> Option<&'static str> {
    Some("open")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn browser_opener() -> Option<&'static str> {
    None
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        build_issue_url, percent_encode, render_bug_command_with, PlatformFacts, TITLE_LIMIT,
    };
    use crate::adapter::{AdapterInstance, FakeAgentAdapter};
    use crate::runtime::state::InlineState;

    #[test]
    fn default_url_encodes_snapshot_and_caps_title() {
        let description = format!("{} extra", "a".repeat(TITLE_LIMIT));
        let url = build_issue_url(&description, "version: 1\nos: linux x86_64");
        assert!(url.contains(&format!("title={}", "a".repeat(TITLE_LIMIT))));
        assert!(!url.contains("extra"));
        assert!(url.contains("diagnostics=version%3A%201%0Aos%3A%20linux%20x86_64"));
        assert_eq!(url.matches("title=").count(), 1);
        assert_eq!(url.matches("diagnostics=").count(), 1);
    }

    #[test]
    fn empty_description_omits_default_title_parameter() {
        let url = build_issue_url("  ", "version: 1");
        assert!(!url.contains("title="), "{url}");
        assert!(url.contains("component=cosh-ng&diagnostics="), "{url}");
    }

    #[test]
    fn percent_encoding_uses_utf8_query_component_contract() {
        assert_eq!(percent_encode("a b+c/中"), "a%20b%2Bc%2F%E4%B8%AD");
    }

    #[test]
    fn panel_mentions_existing_logs_and_never_calls_opener_without_terminal() {
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let state = InlineState::default();
        let mut output = Vec::new();
        render_bug_command_with(
            "failed once",
            &adapter,
            &state,
            &PlatformFacts::default(),
            Some(Path::new("/home/test/.copilot-shell/logs")),
            true,
            false,
            |_| panic!("opener must not run without a terminal"),
            &mut output,
        )
        .expect("render bug panel");
        let text = String::from_utf8(output).expect("utf-8 panel");
        assert!(text.contains("/home/test/.copilot-shell/logs"), "{text}");
        assert!(text.contains("copy the link"), "{text}");
        let url_line = text.lines().last().expect("url line after the panel");
        assert!(
            url_line.starts_with("https://github.com/agentic-os-org/ANOLISA/issues/new?"),
            "{text}"
        );
        assert!(url_line.contains("title=failed%20once"), "{url_line}");
        assert!(url_line.contains("diagnostics=version%3A%20"), "{url_line}");
    }

    #[test]
    fn panel_omits_missing_logs_and_reports_successful_open() {
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let state = InlineState::default();
        let mut output = Vec::new();
        render_bug_command_with(
            "failed once",
            &adapter,
            &state,
            &PlatformFacts::default(),
            Some(Path::new("/home/test/.copilot-shell/logs")),
            false,
            true,
            |_| true,
            &mut output,
        )
        .expect("render bug panel");
        let text = String::from_utf8(output).expect("utf-8 panel");
        assert!(!text.contains(".copilot-shell/logs"), "{text}");
        assert!(text.contains("Opening the link"), "{text}");
    }
}
