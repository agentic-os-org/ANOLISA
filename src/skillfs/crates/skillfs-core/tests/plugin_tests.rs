//! Package registration, projection, and pinned configuration behavior.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::sync::Arc;

use serde_json::json;
use skillfs_core::plugin::{
    DiagnosticSeverity, LoadedPackage, PackageStore, PluginError, check_package_resource_path,
    is_package_root_entry_visible, load_package,
};
use skillfs_core::{DEFAULT_MAX_SKILL_SIZE, ParseStatus};

const MANIFEST_SCHEMA: &str = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";
const MCP_SCHEMA: &str = "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json";

fn manifest(root: &Path, name: &str) -> Vec<u8> {
    let contents = json!({
        "$schema": MANIFEST_SCHEMA,
        "name": name,
        "extensions": {"local-client": {}, "unimplemented": 4}
    });
    let bytes = format!("\u{feff}{contents}\n").into_bytes();
    fs::write(root.join("plugin.json"), &bytes).unwrap();
    bytes
}

fn load(root: &Path) -> LoadedPackage {
    let validation = load_package(root, None);
    validation
        .package
        .unwrap_or_else(|| panic!("{:?}", validation.diagnostics))
}

#[test]
fn package_store_uses_manifest_name_and_allows_distinct_overrides() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    manifest(first.path(), "my-package");
    manifest(second.path(), "my-package");

    let mut packages = PackageStore::new();
    packages.register(load(first.path()), None).unwrap();
    let error = packages.register(load(second.path()), None).unwrap_err();
    let first = first.path().canonicalize().unwrap();
    let second_root = second.path().canonicalize().unwrap();
    assert!(matches!(
        error,
        PluginError::DuplicatePackageId { id, existing, incoming }
        if id == "my-package" && existing == first && incoming == second_root
    ));
    assert_eq!(packages.get("my-package").unwrap().root, first);
    packages
        .register(load(second.path()), Some("alternate"))
        .unwrap();
    assert_eq!(packages.get("alternate").unwrap().root, second_root);
    assert_eq!(
        packages.iter().map(|(id, _)| id).collect::<Vec<_>>(),
        ["alternate", "my-package"]
    );
}

#[test]
fn pins_original_manifests_and_filters_only_applicable_root_entries() {
    let root = tempfile::tempdir().unwrap();
    let manifest_bytes = manifest(root.path(), "package");
    let mcp = json!({
        "$schema": MCP_SCHEMA,
        "mcpServers": {"bad": {"type": "stdio"}, "good": {"type": "stdio", "command": "node"}}
    });
    let mcp_bytes = format!("\u{feff}{mcp}  \n").into_bytes();
    fs::write(root.path().join("mcp.json"), &mcp_bytes).unwrap();

    let pkg = load(root.path());
    let captured = Arc::clone(pkg.manifest_bytes());
    assert_eq!(pkg.servers.len(), 1);
    assert_eq!(pkg.servers[0].name, "good");
    fs::write(root.path().join("mcp.json"), "{}").unwrap();
    fs::write(root.path().join("plugin.json"), "{}").unwrap();
    assert_eq!(&*captured, manifest_bytes);
    assert_eq!(&**pkg.mcp_bytes().unwrap(), mcp_bytes);
    assert!(is_package_root_entry_visible(&pkg, "mcp.json", false));

    for name in [".claude-plugin", ".mcp.json", ".gitignore"] {
        assert!(!is_package_root_entry_visible(&pkg, name, true), "{name}");
        assert!(!is_package_root_entry_visible(&pkg, name, false), "{name}");
    }
    for name in [
        "local-client",
        "unimplemented",
        "org.example",
        "dev.a",
        "io.a-9.x",
    ] {
        assert!(!is_package_root_entry_visible(&pkg, name, true), "{name}");
        assert!(is_package_root_entry_visible(&pkg, name, false), "{name}");
    }
    for name in ["plugin.json", "README.md", "mise.toml"] {
        assert!(is_package_root_entry_visible(&pkg, name, false), "{name}");
    }
    for name in ["scripts", "assets", "Name.With.Uppercase", "org..example"] {
        assert!(is_package_root_entry_visible(&pkg, name, true), "{name}");
    }

    manifest(root.path(), "package");
    let disabled = load(root.path());
    assert!(disabled.mcp_bytes().is_none());
    assert!(!is_package_root_entry_visible(&disabled, "mcp.json", false));
}

#[test]
fn containment_resolves_inside_links_and_missing_suffixes() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("pkg");
    fs::create_dir_all(root.join("actual")).unwrap();
    symlink("actual", root.join("alias")).unwrap();
    symlink("../missing-outside", root.join("outside")).unwrap();
    symlink("loop", root.join("loop")).unwrap();
    let canonical_root = root.canonicalize().unwrap();

    assert_eq!(
        check_package_resource_path(&root, Path::new("alias/missing/../new")).unwrap(),
        canonical_root.join("actual/new")
    );
    for path in ["outside/file", "alias/../../elsewhere", "../pkg/actual"] {
        assert!(
            matches!(
                check_package_resource_path(&root, Path::new(path)),
                Err(PluginError::OutsideRoot { .. })
            ),
            "{path}"
        );
    }
    assert!(check_package_resource_path(&root, Path::new("loop")).is_err());
    assert!(matches!(
        check_package_resource_path(&root, &canonical_root),
        Err(PluginError::AbsoluteResource(_))
    ));
}

#[test]
fn strict_projection_normalizes_name_and_preserves_scalar_spelling() {
    let root = tempfile::tempdir().unwrap();
    manifest(root.path(), "package");
    let leaf = "e\u{301}cole-01";
    let path = root.path().join("skills").join(leaf).join("SKILL.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        "\u{feff}--- \r\nname: école-０１\r\ndescription: true\r\nlicense: null\r\n\
         allowed-tools: Read\r\ncompatibility: 1.0\r\nmetadata: {01: 0x10, no: null}\r\n---\r\nBody.",
    )
    .unwrap();

    let package = load(root.path());
    let skill = &package.skills[0];
    assert_eq!(skill.name, leaf);
    assert_eq!(skill.path, format!("skills/{leaf}/SKILL.md"));
    let entry = skill.to_skill_entry();
    assert_eq!(entry.metadata.name, leaf);
    assert_eq!(entry.metadata.description, "true");
    assert_eq!(entry.source_path, path.canonicalize().unwrap());
    assert_eq!(entry.body, "Body.");
    assert!(matches!(entry.parse_status, ParseStatus::Ok));
}

#[test]
fn bounds_manifest_mcp_and_skill_reads() {
    let root = tempfile::tempdir().unwrap();
    let manifest_bytes = manifest(root.path(), "package");
    let mut oversized = manifest_bytes.clone();
    oversized.resize(DEFAULT_MAX_SKILL_SIZE + 1, b' ');
    fs::write(root.path().join("plugin.json"), &oversized).unwrap();
    let result = load_package(root.path(), None);
    assert!(!result.accepted());
    let [diagnostic] = result.diagnostics.as_slice() else {
        panic!("expected one manifest diagnostic: {:?}", result.diagnostics);
    };
    assert_eq!(diagnostic.severity, DiagnosticSeverity::RejectPlugin);
    assert_eq!(diagnostic.path, "plugin.json");
    assert_eq!(diagnostic.json_pointer, "");
    assert_eq!(diagnostic.spec_ref, "§5.2");
    assert!(diagnostic.message.contains("1 MiB"), "{diagnostic:?}");

    fs::write(root.path().join("plugin.json"), manifest_bytes).unwrap();
    let mut mcp = json!({"$schema": MCP_SCHEMA, "mcpServers": {}})
        .to_string()
        .into_bytes();
    mcp.resize(DEFAULT_MAX_SKILL_SIZE, b' ');
    fs::write(root.path().join("mcp.json"), &mcp).unwrap();
    assert_eq!(&**load(root.path()).mcp_bytes().unwrap(), mcp);
    mcp.push(b' ');
    fs::write(root.path().join("mcp.json"), mcp).unwrap();
    let mut skill = b"---\nname: large\ndescription: a skill\n---\n".to_vec();
    skill.resize(DEFAULT_MAX_SKILL_SIZE + 1, b' ');
    write_skill(root.path(), "large", skill);
    write_skill(
        root.path(),
        "small",
        "---\nname: small\ndescription: a skill\n---\n",
    );
    let result = load_package(root.path(), None);
    assert!(result.accepted());
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|d| (
                d.severity,
                d.path.as_str(),
                d.json_pointer.as_str(),
                d.spec_ref
            ))
            .collect::<Vec<_>>(),
        [
            (
                DiagnosticSeverity::SkipSkill,
                "skills/large/SKILL.md",
                "",
                "§7.1"
            ),
            (
                DiagnosticSeverity::DisableComponent,
                "mcp.json",
                "",
                "§7.2.2"
            )
        ]
    );
    for diagnostic in &result.diagnostics {
        assert!(diagnostic.message.contains("1 MiB"), "{diagnostic:?}");
    }
    let package = result.package.unwrap();
    assert!(package.mcp_bytes().is_none());
    assert!(package.servers.is_empty());
    let [skill] = package.skills.as_slice() else {
        panic!("expected one valid skill: {:?}", package.skills);
    };
    assert_eq!(skill.name, "small");
}

fn write_skill(root: &Path, name: &str, content: impl AsRef<[u8]>) {
    let directory = root.join("skills").join(name);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("SKILL.md"), content).unwrap();
}

#[test]
fn follows_contained_links_and_projects_resources_beside_the_skill() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    manifest(&root_path, "package");
    fs::rename(
        root_path.join("plugin.json"),
        root_path.join("manifest.json"),
    )
    .unwrap();
    symlink(
        root_path.join("manifest.json"),
        root_path.join("plugin.json"),
    )
    .unwrap();

    fs::create_dir_all(root_path.join("catalog/manual")).unwrap();
    fs::create_dir(root_path.join("shared")).unwrap();
    symlink(root_path.join("catalog"), root_path.join("skills")).unwrap();
    let document = root_path.join("shared/manual.md");
    fs::write(
        &document,
        "---\nname: manual\ndescription: Manual\n---\nBody",
    )
    .unwrap();
    let directory = root_path.join("catalog/manual");
    symlink("../../shared/manual.md", directory.join("SKILL.md")).unwrap();
    fs::write(directory.join("notes.txt"), "Companion notes").unwrap();
    let mcp = root_path.join("shared/mcp.json");
    fs::write(
        &mcp,
        format!(r#"{{"$schema":"{MCP_SCHEMA}","mcpServers":{{}}}}"#),
    )
    .unwrap();
    symlink("shared/mcp.json", root_path.join("mcp.json")).unwrap();

    let package = load(root.path());
    assert!(package.mcp_bytes().is_some());
    assert_eq!(package.skills.len(), 1);
    let skill = &package.skills[0];
    assert_eq!(skill.directory, directory);
    assert_eq!(skill.document, document);
    let entry = skill.to_skill_entry();
    assert_eq!(entry.source_path, directory.join("SKILL.md"));
    assert_eq!(
        fs::read_to_string(entry.source_path.parent().unwrap().join("notes.txt")).unwrap(),
        "Companion notes"
    );
}

#[test]
fn registry_rejects_ids_that_cannot_be_used_as_one_visible_path_segment() {
    let root = tempfile::tempdir().unwrap();
    manifest(root.path(), "package");
    let mut store = PackageStore::new();
    let too_long = "é".repeat(128);
    for id in [
        "", ".", "..", ".hidden", "a/b", "a\\b", "a\n", "a\0b", &too_long,
    ] {
        assert!(
            matches!(store.register(load(root.path()), Some(id)), Err(PluginError::InvalidPackageId(found)) if found == id),
            "{id:?}"
        );
    }
    assert_eq!(store.iter().count(), 0);
    let longest_allowed = format!("{}1", "é".repeat(127));
    store
        .register(load(root.path()), Some(&longest_allowed))
        .unwrap();
    assert!(store.get(&longest_allowed).is_some());
}

#[test]
fn only_discovered_standard_entries_are_visible() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("package");
    let skills = root.join("skills");
    fs::create_dir(&root).unwrap();
    manifest(&root, "package");
    let before = load(&root);
    fs::create_dir(&skills).unwrap();
    fs::write(
        root.join("mcp.json"),
        format!(r#"{{"$schema":"{MCP_SCHEMA}","mcpServers":{{}}}}"#),
    )
    .unwrap();
    assert!(!is_package_root_entry_visible(&before, "mcp.json", false));
    assert!(!is_package_root_entry_visible(&before, "skills", true));

    let after = load(&root);
    assert!(is_package_root_entry_visible(&after, "plugin.json", false));
    assert!(is_package_root_entry_visible(&after, "skills", true));
    assert!(is_package_root_entry_visible(&after, "mcp.json", false));
    assert!(!is_package_root_entry_visible(&after, "plugin.json", true));
    assert!(!is_package_root_entry_visible(&after, "mcp.json", true));
    assert!(!is_package_root_entry_visible(&after, "skills", false));

    fs::remove_dir(&skills).unwrap();
    fs::write(&skills, "not a directory").unwrap();
    let result = load_package(&root, None);
    assert_eq!(
        result.diagnostics[0].severity,
        DiagnosticSeverity::DisableComponent
    );
    let disabled = result.package.unwrap();
    assert!(!is_package_root_entry_visible(&disabled, "skills", true));
    assert!(!is_package_root_entry_visible(&disabled, "skills", false));

    fs::remove_file(&skills).unwrap();
    let external = fixture.path().join("other");
    fs::create_dir(&external).unwrap();
    symlink(external, &skills).unwrap();
    let result = load_package(&root, None);
    assert_eq!(result.diagnostics[0].spec_ref, "§4.1");
    assert!(!is_package_root_entry_visible(
        &result.package.unwrap(),
        "skills",
        true
    ));
}

#[test]
fn isolates_overflowing_json_numbers_and_preserves_last_wins_keys() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("plugin.json"),
        format!(
            r#"{{"$schema":"{MANIFEST_SCHEMA}","name":1e400,"name":"package","extra":1e400,
                 "extensions":{{"org.example":1e400}}}}"#
        ),
    )
    .unwrap();
    fs::write(
        root.path().join("mcp.json"),
        format!(
            r#"{{"$schema":"{MCP_SCHEMA}","mcpServers":{{
                 "bad":{{"type":"stdio","command":1e400}},
                 "good":{{"type":"stdio","command":1e400,"command":"node",
                          "env":{{"ENV":1e400,"ENV":"valid"}}}}
               }}}}"#
        ),
    )
    .unwrap();
    let result = load_package(root.path(), None);
    assert_eq!(result.diagnostics.len(), 2);
    assert_eq!(result.diagnostics[0].severity, DiagnosticSeverity::Warn);
    assert_eq!(result.diagnostics[0].json_pointer, "/extra");
    assert_eq!(
        result.diagnostics[1].severity,
        DiagnosticSeverity::SkipServer
    );
    assert_eq!(
        result.diagnostics[1].json_pointer,
        "/mcpServers/bad/command"
    );
    let package = result.package.unwrap();
    assert_eq!(package.servers.len(), 1);
    assert_eq!(package.servers[0].name, "good");
    assert!(package.mcp_bytes().is_some());
    let manifest_json = serde_json::to_string(&package.manifest).unwrap();
    assert!(manifest_json.contains(r#""org.example":1e400"#));
}

#[test]
fn raw_json_still_rejects_non_json_syntax() {
    let root = tempfile::tempdir().unwrap();
    for invalid in ["NaN", "Infinity", "1e", "1 2"] {
        fs::write(
            root.path().join("plugin.json"),
            format!(r#"{{"$schema":"{MANIFEST_SCHEMA}","name":"package","extra":{invalid}}}"#),
        )
        .unwrap();
        let rejected = load_package(root.path(), None);
        assert!(!rejected.accepted(), "{invalid}");
        assert_eq!(rejected.diagnostics[0].spec_ref, "§5.2");
        manifest(root.path(), "package");
        fs::write(
            root.path().join("mcp.json"),
            format!(r#"{{"$schema":"{MCP_SCHEMA}","mcpServers":{{"bad":{invalid}}}}}"#),
        )
        .unwrap();
        let accepted = load_package(root.path(), None);
        assert!(accepted.accepted(), "{invalid}");
        assert_eq!(
            accepted.diagnostics[0].severity,
            DiagnosticSeverity::DisableComponent
        );
        assert_eq!(accepted.diagnostics[0].spec_ref, "§7.2.2");
    }
}

#[test]
fn accepts_tabs_after_skill_frontmatter_fences() {
    let root = tempfile::tempdir().unwrap();
    manifest(root.path(), "package");
    write_skill(
        root.path(),
        "tabs",
        "---\t\nname: tabs\ndescription: Example\n---\t\nBody.",
    );

    let result = load_package(root.path(), None);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let package = result.package.unwrap();
    assert_eq!(package.skills.len(), 1);
    assert_eq!(package.skills[0].to_skill_entry().body, "Body.");
}

#[test]
fn projects_cr_only_skill_body_and_markdown_sections() {
    let root = tempfile::tempdir().unwrap();
    manifest(root.path(), "package");
    write_skill(
        root.path(),
        "cr",
        "---\rname: cr\rdescription: hi\r---\r---\rBody\r\r## Parameters\r\r- `key` (string, required): Value\r",
    );
    let package = load(root.path());
    assert_eq!(package.skills.len(), 1);
    let entry = package.skills[0].to_skill_entry();
    assert_eq!(
        entry.body,
        "---\nBody\n\n## Parameters\n\n- `key` (string, required): Value\n"
    );
    assert_eq!(entry.parameters.len(), 1);
    assert_eq!(entry.parameters[0].name, "key");
    assert_eq!(entry.metadata.description, "hi");
}

#[test]
fn rejects_combining_marks_in_names_and_points_to_empty_keys() {
    let root = tempfile::tempdir().unwrap();
    manifest(root.path(), "package");
    write_skill(
        root.path(),
        "mark\u{093e}",
        "---\nname: mark\u{093e}\ndescription: ok\n---\n",
    );
    write_skill(
        root.path(),
        "empty-key",
        "---\nname: empty-key\ndescription: ok\n\"\": ignored\n---\n",
    );
    write_skill(root.path(), "字9", "---\nname: 字9\ndescription: ok\n---\n");

    let result = load_package(root.path(), None);
    assert_eq!(result.diagnostics.len(), 2);
    let empty_key = &result.diagnostics[0];
    assert_eq!(empty_key.severity, DiagnosticSeverity::SkipSkill);
    assert_eq!(empty_key.path, "skills/empty-key/SKILL.md");
    assert_eq!(empty_key.json_pointer, "/");
    let mark = &result.diagnostics[1];
    assert_eq!(mark.path, "skills/mark\u{093e}/SKILL.md");
    assert_eq!(mark.json_pointer, "/name");
    assert_eq!(result.package.unwrap().skills[0].name, "字9");
}

#[cfg(target_os = "linux")]
#[test]
fn diagnoses_non_utf8_names_only_for_directories_with_skill_documents() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let root = tempfile::tempdir().unwrap();
    manifest(root.path(), "package");
    let skills = root.path().join("skills");
    fs::create_dir(&skills).unwrap();
    fs::write(skills.join(OsStr::from_bytes(b"data-\xff")), "resource").unwrap();
    fs::create_dir(skills.join(OsStr::from_bytes(b"assets-\xff"))).unwrap();
    let assets = skills.join(OsStr::from_bytes(b"assets2-\xff"));
    fs::create_dir(&assets).unwrap();
    fs::write(assets.join("README.md"), "Resources").unwrap();
    let result = load_package(root.path(), None);
    assert!(result.accepted());
    assert!(result.diagnostics.is_empty());

    let candidate = skills.join(OsStr::from_bytes(b"candidate-\xff"));
    fs::create_dir(&candidate).unwrap();
    fs::write(
        candidate.join("SKILL.md"),
        "---\nname: candidate\ndescription: Candidate\n---\n",
    )
    .unwrap();
    let result = load_package(root.path(), None);
    assert!(result.accepted());
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(
        result.diagnostics[0].path,
        "skills/candidate-\u{fffd}/SKILL.md"
    );
    assert_eq!(
        result.diagnostics[0].severity,
        DiagnosticSeverity::SkipSkill
    );
}
