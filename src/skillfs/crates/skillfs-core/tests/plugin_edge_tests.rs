//! Package discovery at filesystem kind and permission boundaries.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::UnixListener;
use std::path::Path;

use serde_json::json;
use skillfs_core::plugin::{
    DiagnosticSeverity, McpTransport, PluginError, PluginManifest, check_package_resource_path,
    is_package_root_entry_visible, load_package,
};

const SCHEMA: &str = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";
const MCP_SCHEMA: &str = "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json";

fn write_manifest(root: &Path) {
    let bytes = json!({"$schema": SCHEMA, "name": "package"}).to_string();
    fs::write(root.join("plugin.json"), bytes).unwrap();
}

fn write_skill(root: &Path, name: &str) {
    let directory = root.join("skills").join(name);
    fs::create_dir_all(&directory).unwrap();
    let body = format!("---\nname: {name}\ndescription: Example\n---\n");
    fs::write(directory.join("SKILL.md"), body).unwrap();
}

#[test]
fn traverses_search_only_roots_and_skill_directories_without_listing_them() {
    // SAFETY: geteuid has no preconditions and does not retain anything.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }

    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("package");
    fs::create_dir(&root).unwrap();
    write_manifest(&root);
    write_skill(&root, "manual");
    write_skill(&root, "blocked");
    let chmod = |path: &Path, mode| {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    };
    let skills = root.join("skills");
    let manual = skills.join("manual");
    let blocked = skills.join("blocked");
    chmod(&root.join("plugin.json"), 0o644);
    chmod(&manual.join("SKILL.md"), 0o644);
    chmod(&blocked.join("SKILL.md"), 0o644);
    chmod(&manual, 0o111);
    chmod(&blocked, 0o000);
    chmod(&root, 0o111);
    let searchable = load_package(&root, None);
    chmod(&root, 0o755);
    chmod(&manual, 0o755);
    chmod(&blocked, 0o755);

    let package = searchable.package.unwrap();
    assert_eq!(package.skills.len(), 1);
    assert_eq!(package.skills[0].name, "manual");
    assert_eq!(searchable.diagnostics.len(), 1);
    let diagnostic = &searchable.diagnostics[0];
    assert_eq!(diagnostic.severity, DiagnosticSeverity::SkipSkill);
    assert_eq!(diagnostic.path, "skills/blocked/SKILL.md");
    assert_eq!(diagnostic.spec_ref, "§7.1");

    chmod(&skills, 0o111);
    let unlistable = load_package(&root, None);
    chmod(&skills, 0o755);
    assert!(unlistable.package.unwrap().skills.is_empty());
    assert_eq!(unlistable.diagnostics.len(), 1);
    let diagnostic = &unlistable.diagnostics[0];
    assert_eq!(diagnostic.severity, DiagnosticSeverity::DisableComponent);
    assert_eq!(diagnostic.path, "skills");
    assert_eq!(diagnostic.message, "cannot list skills directory");
}

#[test]
fn trailing_separators_and_dot_segments_require_directories() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    write_manifest(&root);
    fs::rename(root.join("plugin.json"), root.join("real.json")).unwrap();

    for target in [
        "real.json/".to_owned(),
        "real.json/.".to_owned(),
        "real.json/..".to_owned(),
        format!("{}/real.json/.", root.display()),
    ] {
        symlink(&target, root.join("plugin.json")).unwrap();
        let result = load_package(&root, None);
        assert!(!result.accepted(), "{target}");
        assert_eq!(result.diagnostics[0].spec_ref, "§5.1");
        fs::remove_file(root.join("plugin.json")).unwrap();
    }
    symlink("real.json/.", root.join("link.json")).unwrap();
    for path in ["real.json/", "real.json/.", "real.json/..", "link.json"] {
        assert!(
            matches!(
                check_package_resource_path(&root, Path::new(path)),
                Err(PluginError::ResolvePath { source, .. })
                if source.kind() == std::io::ErrorKind::NotADirectory
            ),
            "{path}"
        );
    }
    assert_eq!(
        check_package_resource_path(&root, Path::new("new-directory/")).unwrap(),
        root.join("new-directory")
    );

    symlink("real.json", root.join("plugin.json")).unwrap();
    fs::create_dir_all(root.join("skills/manual")).unwrap();
    fs::create_dir(root.join("skills/shared")).unwrap();
    fs::write(
        root.join("skills/shared/real.md"),
        "---\nname: manual\ndescription: Manual\n---\n",
    )
    .unwrap();
    symlink("../shared/real.md/", root.join("skills/manual/SKILL.md")).unwrap();
    let result = load_package(&root, None);
    assert!(result.package.unwrap().skills.is_empty());
    assert!(result.diagnostics.is_empty());
}

#[test]
fn extension_names_do_not_hide_validated_skills_directory() {
    let root = tempfile::tempdir().unwrap();
    let contents = json!({
        "$schema": SCHEMA,
        "name": "package",
        "extensions": {"skills": {}, "hooks": {}}
    });
    fs::write(root.path().join("plugin.json"), contents.to_string()).unwrap();
    write_skill(root.path(), "manual");
    fs::create_dir(root.path().join("hooks")).unwrap();
    let result = load_package(root.path(), None);
    let package = result.package.unwrap();
    assert_eq!(package.skills.len(), 1);
    assert!(is_package_root_entry_visible(&package, "skills", true));
    assert!(!is_package_root_entry_visible(&package, "skills", false));
    assert!(!is_package_root_entry_visible(&package, "hooks", true));
}

#[test]
fn manifest_json_round_trips_with_raw_extensions() {
    let input = r#"{
        "$schema": "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json",
        "name": "package",
        "version": "1.2.3",
        "description": "Example",
        "author": {"name": "Example Author", "email": "author@example.com", "url": "https://example.com/author"},
        "homepage": "https://example.com",
        "repository": "https://example.com/source",
        "license": "MIT",
        "keywords": ["example"],
        "extensions": {"com.example": { "list": [true, null, 42], "object": {} }}
    }"#;
    let manifest: PluginManifest = serde_json::from_str(input).unwrap();
    let encoded = serde_json::to_string(&manifest).unwrap();
    let decoded: PluginManifest = serde_json::from_str(&encoded).unwrap();

    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&encoded).unwrap(),
        serde_json::from_str::<serde_json::Value>(input).unwrap()
    );
    assert_eq!(decoded.schema, SCHEMA);
    assert_eq!(
        decoded.extensions.unwrap()["com.example"].get(),
        r#"{ "list": [true, null, 42], "object": {} }"#
    );
}

#[test]
fn sockets_are_wrong_kind_at_fixed_documents_and_skill_markers() {
    let root = tempfile::tempdir().unwrap();
    let manifest = root.path().join("plugin.json");
    let socket = UnixListener::bind(&manifest).unwrap();
    let result = load_package(root.path(), None);
    assert!(!result.accepted());
    assert_eq!(
        result.diagnostics[0].severity,
        DiagnosticSeverity::RejectPlugin
    );
    assert_eq!(result.diagnostics[0].spec_ref, "§5.1");
    drop(socket);
    fs::remove_file(manifest).unwrap();
    write_manifest(root.path());

    let skill = root.path().join("skills/sock");
    fs::create_dir_all(&skill).unwrap();
    let _skill = UnixListener::bind(skill.join("SKILL.md")).unwrap();
    let result = load_package(root.path(), None);
    assert!(result.package.unwrap().skills.is_empty());
    assert!(result.diagnostics.is_empty());

    let _mcp = UnixListener::bind(root.path().join("mcp.json")).unwrap();
    let result = load_package(root.path(), None);
    assert!(result.accepted());
    assert_eq!(result.diagnostics.len(), 1);
    let diagnostic = &result.diagnostics[0];
    assert_eq!(diagnostic.severity, DiagnosticSeverity::DisableComponent);
    assert_eq!(diagnostic.path, "mcp.json");
    assert_eq!(diagnostic.spec_ref, "§6.2");
}

#[test]
fn rejects_percent_suffixes_in_bracketed_ipv6_urls() {
    let root = tempfile::tempdir().unwrap();
    write_manifest(root.path());
    let mcp = json!({
        "$schema": MCP_SCHEMA,
        "mcpServers": {
            "escape": {"type": "streamable-http", "url": "http://[::1%20]/mcp"},
            "loopback": {"type": "streamable-http", "url": "http://[::1]/mcp"},
            "loopback-port": {"type": "sse", "url": "http://[::1]:1234/sse"},
            "remote": {"type": "streamable-http", "url": "https://[2001:db8::1]/mcp"},
            "zone": {"type": "streamable-http", "url": "http://[::1%25lo0]/mcp"}
        }
    });
    fs::write(root.path().join("mcp.json"), mcp.to_string()).unwrap();

    let result = load_package(root.path(), None);
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.severity, diagnostic.json_pointer.as_str()))
            .collect::<Vec<_>>(),
        [
            (DiagnosticSeverity::SkipServer, "/mcpServers/escape/url"),
            (DiagnosticSeverity::SkipServer, "/mcpServers/zone/url")
        ]
    );
    let package = result.package.unwrap();
    assert_eq!(
        package
            .servers
            .iter()
            .map(|server| server.name.as_str())
            .collect::<Vec<_>>(),
        ["loopback", "loopback-port", "remote"]
    );
}

#[test]
fn cwd_requires_directories_and_resources_reject_traversal_through_regular_files() {
    let root = tempfile::tempdir().unwrap();
    write_manifest(root.path());
    fs::write(root.path().join("plain"), "not a directory").unwrap();
    let mcp = json!({
        "$schema": MCP_SCHEMA,
        "mcpServers": {
            "a": {"type": "stdio", "command": "tool", "cwd": "./plain/child"},
            "b": {"type": "stdio", "command": "tool", "cwd": "./plain/child/"},
            "c": {"type": "stdio", "command": "tool", "cwd": "./future/child"},
            "d": {"type": "stdio", "command": "tool", "cwd": "./plain"},
            "e": {"type": "stdio", "command": "tool", "cwd": "${PLUGIN_ROOT}/plain"},
            "f": {"type": "stdio", "command": "tool", "cwd": "${PLUGIN_ROOT}"},
            "g": {"type": "stdio", "command": "tool", "cwd": "${PLUGIN_DATA}/plain"},
            "h": {"type": "stdio", "command": "tool", "cwd": "${PLUGIN_DATA}/future/child"}
        }
    });
    fs::write(root.path().join("mcp.json"), mcp.to_string()).unwrap();

    let result = load_package(root.path(), Some(root.path()));
    let package = result.package.unwrap();
    assert_eq!(
        package
            .servers
            .iter()
            .map(|server| (server.name.as_str(), server.transport))
            .collect::<Vec<_>>(),
        [
            ("c", McpTransport::Stdio),
            ("f", McpTransport::Stdio),
            ("h", McpTransport::Stdio)
        ]
    );
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.json_pointer.as_str())
            .collect::<Vec<_>>(),
        [
            "/mcpServers/a/cwd",
            "/mcpServers/b/cwd",
            "/mcpServers/d/cwd",
            "/mcpServers/e/cwd",
            "/mcpServers/g/cwd"
        ]
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity == DiagnosticSeverity::SkipServer)
    );
    assert!(matches!(
        check_package_resource_path(root.path(), Path::new("plain/child")),
        Err(PluginError::ResolvePath { source, .. })
        if source.kind() == std::io::ErrorKind::NotADirectory
    ));
}

#[test]
fn cwd_and_resource_ascent_require_directory_search_permission() {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }

    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    write_manifest(&root);
    let locked = root.join("locked");
    let real = root.join("real");
    fs::create_dir(&locked).unwrap();
    fs::create_dir(&real).unwrap();
    let mcp = json!({
        "$schema": MCP_SCHEMA,
        "mcpServers": {
            "ascent": {"type": "stdio", "command": "tool", "cwd": "./locked/../real"},
            "child": {"type": "stdio", "command": "tool", "cwd": "./locked/future"},
            "target": {"type": "stdio", "command": "tool", "cwd": "./locked"}
        }
    });
    fs::write(root.join("mcp.json"), mcp.to_string()).unwrap();
    let path = Path::new("locked/../real");
    let chmod = |mode| fs::set_permissions(&locked, fs::Permissions::from_mode(mode)).unwrap();

    chmod(0o000);
    let rejected = load_package(&root, None);
    let rejected_resource = check_package_resource_path(&root, path);
    chmod(0o111);
    let accepted = load_package(&root, None);
    let accepted_resource = check_package_resource_path(&root, path);
    chmod(0o755);

    assert!(rejected.package.unwrap().servers.is_empty());
    assert_eq!(rejected.diagnostics.len(), 3);
    assert!(rejected.diagnostics.iter().all(|diagnostic| {
        diagnostic.severity == DiagnosticSeverity::SkipServer
            && diagnostic.json_pointer.ends_with("/cwd")
    }));
    assert!(matches!(
        rejected_resource,
        Err(PluginError::ResolvePath { source, .. })
        if source.kind() == std::io::ErrorKind::PermissionDenied
    ));
    assert_eq!(accepted.package.unwrap().servers.len(), 3);
    assert!(accepted.diagnostics.is_empty());
    assert_eq!(accepted_resource.unwrap(), real);
}

#[test]
fn parent_ascent_requires_search_permission_on_traversed_directory() {
    // SAFETY: geteuid has no preconditions and does not retain anything.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }

    let root = tempfile::tempdir().unwrap();
    write_manifest(root.path());
    fs::rename(
        root.path().join("plugin.json"),
        root.path().join("real.json"),
    )
    .unwrap();
    let locked = root.path().join("locked");
    fs::create_dir(&locked).unwrap();
    symlink("locked/../real.json", root.path().join("plugin.json")).unwrap();
    let chmod = |mode| fs::set_permissions(&locked, fs::Permissions::from_mode(mode)).unwrap();

    chmod(0o000);
    let rejected = load_package(root.path(), None);
    assert!(!rejected.accepted());
    assert_eq!(
        rejected.diagnostics[0].severity,
        DiagnosticSeverity::RejectPlugin
    );

    chmod(0o111);
    let accepted = load_package(root.path(), None);
    chmod(0o755);
    assert!(accepted.accepted());
}
