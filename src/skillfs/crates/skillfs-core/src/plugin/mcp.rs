//! Validate a fixed MCP configuration, then validate its servers independently.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock};

use regex::Regex;
use serde::Serialize;
use serde_json::value::RawValue;

use super::containment::{
    Kind, Opened, PackageRoot, ReadError, inside_plugin_root, inside_plugin_root_dir,
};
use super::{
    DiagnosticSeverity, JsonObject, PluginComponent, PluginDiagnostic, fixed_location, json_as,
    json_object, json_string_map, read_json,
};

const SCHEMA: &str = "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json";
const ROOT: &str = "${PLUGIN_ROOT}";
const DATA: &str = "${PLUGIN_DATA}";
const VIRTUAL_DATA: &str = "/__skillfs_empty_data__";

/// Transport declared by a valid MCP server configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpTransport {
    /// A local subprocess communicating over standard streams.
    Stdio,
    /// Streamable HTTP.
    StreamableHttp,
    /// Legacy HTTP with server-sent events.
    Sse,
}

impl McpTransport {
    /// Agent Plugins transport name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::StreamableHttp => "streamable-http",
            Self::Sse => "sse",
        }
    }
}

/// A server that passed static configuration validation.
#[derive(Debug, Serialize)]
pub struct McpServer {
    /// Key from the `mcpServers` object.
    pub name: String,
    /// Declared transport.
    #[serde(rename = "type")]
    pub transport: McpTransport,
}

#[derive(Default)]
pub(super) struct Discovery {
    pub(super) bytes: Option<Arc<[u8]>>,
    pub(super) servers: Vec<McpServer>,
}

pub(super) fn discover(
    root: &PackageRoot,
    plugin_data: Option<&Path>,
    diagnostics: &mut Vec<PluginDiagnostic>,
) -> Discovery {
    let Some(entry) = fixed_location(
        root,
        "mcp.json",
        PluginComponent::Mcp,
        Kind::RegularFile,
        diagnostics,
    ) else {
        return Discovery::default();
    };

    match load(&entry, root, plugin_data, diagnostics) {
        Ok(discovery) => discovery,
        Err(error) => {
            diagnostics.push(PluginDiagnostic::new(
                DiagnosticSeverity::DisableComponent,
                PluginComponent::Mcp,
                "mcp.json",
                &error.keys.iter().map(String::as_str).collect::<Vec<_>>(),
                error.spec_ref,
                error.message,
            ));
            Discovery::default()
        }
    }
}

fn load(
    entry: &Opened,
    root: &PackageRoot,
    plugin_data: Option<&Path>,
    diagnostics: &mut Vec<PluginDiagnostic>,
) -> Result<Discovery, Issue> {
    let invalid_json = || Issue::new(&[], "§7.2.2", "cannot read valid MCP JSON");
    let (bytes, _) = entry.read_regular(root).map_err(|error| match error {
        ReadError::TooLarge => Issue::new(&[], "§7.2.2", "MCP configuration exceeds 1 MiB limit"),
        ReadError::Io(_) => invalid_json(),
    })?;
    let value = read_json(&bytes).ok_or_else(invalid_json)?;
    let object = json_object(value)
        .ok_or_else(|| Issue::config(&[], "MCP configuration must be an object"))?;
    if object
        .get("$schema")
        .and_then(|value| json_as::<String>(value))
        .as_deref()
        != Some(SCHEMA)
    {
        return Err(Issue::new(
            &["$schema"],
            "§10.1",
            "unsupported or mismatched MCP schema",
        ));
    }
    if let Some(extra) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "$schema" | "mcpServers"))
    {
        return Err(Issue::config(&[extra], "unknown top-level MCP field"));
    }
    let entries = object
        .get("mcpServers")
        .and_then(|value| json_object(value))
        .ok_or_else(|| Issue::config(&["mcpServers"], "mcpServers must be an object"))?;

    let data = match plugin_data {
        Some(path) => DataRoot::Physical(
            std::path::absolute(path)
                .ok()
                .and_then(|absolute| inside_plugin_root(&absolute, Path::new("/")).ok()),
        ),
        None => DataRoot::Virtual,
    };
    let mut servers = Vec::new();
    for (name, entry) in entries {
        match validate_server(entry, root.path(), &data) {
            Ok(transport) => servers.push(McpServer { name, transport }),
            Err(error) => {
                let mut keys = vec!["mcpServers", name.as_str()];
                keys.extend(error.keys.iter().map(String::as_str));
                diagnostics.push(PluginDiagnostic::new(
                    DiagnosticSeverity::SkipServer,
                    PluginComponent::Mcp,
                    "mcp.json",
                    &keys,
                    error.spec_ref,
                    error.message,
                ));
            }
        }
    }
    Ok(Discovery {
        bytes: Some(bytes.into()),
        servers,
    })
}

struct Issue {
    keys: Vec<String>,
    spec_ref: &'static str,
    message: &'static str,
}

impl Issue {
    fn new(keys: &[&str], spec_ref: &'static str, message: &'static str) -> Self {
        Self {
            keys: keys.iter().map(|key| (*key).to_owned()).collect(),
            spec_ref,
            message,
        }
    }

    fn config(keys: &[&str], message: &'static str) -> Self {
        Self::new(keys, "§7.2.1", message)
    }
}

fn validate_server(value: &RawValue, root: &Path, data: &DataRoot) -> Result<McpTransport, Issue> {
    let entry =
        json_object(value).ok_or_else(|| Issue::config(&[], "server entry must be an object"))?;
    let transport = match entry
        .get("type")
        .and_then(|value| json_as::<String>(value))
        .as_deref()
    {
        Some("stdio") => McpTransport::Stdio,
        Some("streamable-http") => McpTransport::StreamableHttp,
        Some("sse") => McpTransport::Sse,
        _ => return Err(Issue::config(&["type"], "missing or unknown transport")),
    };
    let allowed: &[&str] = match transport {
        McpTransport::Stdio => &["type", "command", "args", "env", "cwd"],
        McpTransport::StreamableHttp | McpTransport::Sse => &["type", "url", "headers"],
    };
    if let Some(extra) = entry.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(Issue::config(
            &[extra],
            "field is not allowed for this transport",
        ));
    }
    match transport {
        McpTransport::Stdio => validate_stdio(&entry, root, data)?,
        McpTransport::StreamableHttp | McpTransport::Sse => validate_remote(&entry)?,
    }
    Ok(transport)
}

fn validate_stdio(entry: &JsonObject<'_>, root: &Path, data: &DataRoot) -> Result<(), Issue> {
    let command = entry
        .get("command")
        .and_then(|value| json_as::<String>(value))
        .filter(|value| valid_command(value))
        .ok_or_else(|| Issue::config(&["command"], "expected one bare or ./ executable token"))?;
    if command.starts_with("./") && inside_plugin_root(&root.join(command), root).is_err() {
        return Err(Issue::new(
            &["command"],
            "§4.1",
            "command fails package containment",
        ));
    }
    if entry
        .get("args")
        .is_some_and(|args| json_as::<Vec<String>>(args).is_none())
    {
        return Err(Issue::config(&["args"], "args must be a list of strings"));
    }
    if let Some(env) = entry.get("env") {
        let env = json_string_map(env)
            .ok_or_else(|| Issue::config(&["env"], "env must be a string map"))?;
        for key in ["PLUGIN_DATA", "PLUGIN_ROOT"] {
            if env.contains_key(key) {
                return Err(Issue::new(
                    &["env", key],
                    "§9.2",
                    "reserved environment variable",
                ));
            }
        }
    }
    if entry.get("cwd").is_some_and(|cwd| {
        !json_as::<String>(cwd).is_some_and(|value| valid_cwd(&value, root, data))
    }) {
        return Err(Issue::config(&["cwd"], "invalid cwd form or containment"));
    }
    Ok(())
}

fn valid_command(value: &str) -> bool {
    if value.is_empty() || value.chars().any(char::is_control) {
        return false;
    }
    if let Some(relative) = value.strip_prefix("./") {
        return !relative.is_empty();
    }
    let drive = matches!(value.as_bytes(), [letter, b':', ..] if letter.is_ascii_alphabetic());
    !drive
        && !matches!(value, "." | "..")
        && !value.contains(['/', '\\'])
        && !value.chars().any(char::is_whitespace)
}

enum DataRoot {
    Virtual,
    Physical(Option<PathBuf>),
}

fn valid_cwd(value: &str, root: &Path, data: &DataRoot) -> bool {
    if value.contains('\0') {
        return false;
    }
    let expanded = expand(value, root, data);
    let Some(expanded) = expanded.as_deref().map(Path::new) else {
        return false;
    };
    if value.starts_with("./") {
        return inside_plugin_root_dir(&root.join(expanded), root).is_ok();
    }
    if has_variable_prefix(value, ROOT) {
        return inside_plugin_root_dir(expanded, root).is_ok();
    }
    if has_variable_prefix(value, DATA) {
        return match data {
            DataRoot::Virtual => inside_virtual_data(expanded),
            DataRoot::Physical(Some(base)) => inside_plugin_root_dir(expanded, base).is_ok(),
            DataRoot::Physical(None) => false,
        };
    }
    false
}

fn has_variable_prefix(value: &str, variable: &str) -> bool {
    value
        .strip_prefix(variable)
        .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('/'))
}

fn expand(value: &str, root: &Path, data: &DataRoot) -> Option<OsString> {
    // Scan only the configured string. A path containing a placeholder-like
    // token in its literal name must be inserted without a second expansion.
    let mut expanded = OsString::new();
    let mut remaining = value;
    while let Some(start) = remaining.find("${") {
        let (literal, placeholder) = remaining.split_at(start);
        expanded.push(literal);
        if let Some(rest) = placeholder.strip_prefix(ROOT) {
            expanded.push(root);
            remaining = rest;
        } else if let Some(rest) = placeholder.strip_prefix(DATA) {
            let base = match data {
                DataRoot::Virtual => Path::new(VIRTUAL_DATA),
                DataRoot::Physical(base) => base.as_deref()?,
            };
            expanded.push(base);
            remaining = rest;
        } else {
            expanded.push("${");
            remaining = &placeholder[2..];
        }
    }
    expanded.push(remaining);
    Some(expanded)
}

fn inside_virtual_data(path: &Path) -> bool {
    let Ok(suffix) = path.strip_prefix(VIRTUAL_DATA) else {
        return false;
    };
    let mut depth = 0;
    for component in suffix.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::ParentDir if depth > 0 => depth -= 1,
            Component::CurDir => {}
            _ => return false,
        }
    }
    true
}

fn validate_remote(entry: &JsonObject<'_>) -> Result<(), Issue> {
    if !entry
        .get("url")
        .and_then(|value| json_as::<String>(value))
        .is_some_and(|value| valid_url(&value))
    {
        return Err(Issue::config(
            &["url"],
            "invalid HTTP(S) endpoint, userinfo, fragment, or non-loopback HTTP",
        ));
    }
    let Some(headers) = entry.get("headers") else {
        return Ok(());
    };
    let headers = json_string_map(headers)
        .ok_or_else(|| Issue::config(&["headers"], "headers must be a string map"))?;
    let mut seen = BTreeSet::new();
    for (key, value) in headers {
        if !valid_header_name(&key)
            || !seen.insert(key.to_ascii_lowercase())
            || !valid_header_value(&value)
        {
            return Err(Issue::config(
                &["headers", &key],
                "invalid or duplicate HTTP header",
            ));
        }
    }
    Ok(())
}

fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_\x60|~".contains(&byte))
}

fn valid_header_value(value: &str) -> bool {
    value == value.trim_matches([' ', '\t'])
        && value
            .chars()
            .all(|c| matches!(c, '\t' | '\x20'..='\x7e' | '\u{80}'..='\u{ff}'))
}

fn valid_url(value: &str) -> bool {
    static URI: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| {
        Regex::new(r"\A(?:[A-Za-z0-9._~:/?\[\]@!$&'()*+,;=-]|%[A-Fa-f0-9]{2})+\z")
    });
    if !URI.as_ref().is_ok_and(|pattern| pattern.is_match(value)) {
        return false;
    }
    let Some((scheme, rest)) = value.split_once("://") else {
        return false;
    };
    let secure = scheme.eq_ignore_ascii_case("https");
    if !secure && !scheme.eq_ignore_ascii_case("http") {
        return false;
    }
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, path_query) = rest.split_at(end);
    if authority.contains('@') || path_query.contains(['[', ']']) {
        return false;
    }
    if let Some(bracketed) = authority.strip_prefix('[') {
        let Some((host, suffix)) = bracketed.split_once(']') else {
            return false;
        };
        if !suffix.is_empty() && !suffix.strip_prefix(':').is_some_and(valid_port) {
            return false;
        }
        return valid_ip_literal(host, secure);
    }
    let (host, port) = authority.split_once(':').unwrap_or((authority, ""));
    if host.is_empty() || host.contains(['[', ']']) || !valid_port(port) {
        return false;
    }
    secure
        || host.eq_ignore_ascii_case("localhost")
        || host.parse::<Ipv4Addr>().is_ok_and(|ip| ip.is_loopback())
}

fn valid_ip_literal(host: &str, secure: bool) -> bool {
    if let Some(future) = host.strip_prefix('v') {
        return secure
            && future.split_once('.').is_some_and(|(version, address)| {
                !version.is_empty()
                    && version.bytes().all(|byte| byte.is_ascii_hexdigit())
                    && !address.is_empty()
                    && address.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:".contains(&byte)
                    })
            });
    }
    host.parse::<Ipv6Addr>()
        .is_ok_and(|ip| secure || ip.is_loopback())
}

fn valid_port(port: &str) -> bool {
    port.is_empty()
        || (port.bytes().all(|byte| byte.is_ascii_digit()) && port.parse::<u16>().is_ok())
}
