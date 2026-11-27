//! Static validation parity with the 158 Agent Plugins V4 conformance cases.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};
use skillfs_core::plugin::load_package;

#[derive(Deserialize)]
struct Fixtures {
    common_files: BTreeMap<String, String>,
    cases: BTreeMap<String, Case>,
}

#[derive(Deserialize)]
struct Case {
    files: BTreeMap<String, Contents>,
    #[serde(default)]
    dirs: Vec<String>,
    #[serde(default)]
    links: BTreeMap<String, String>,
    expected: Value,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Contents {
    Text(String),
    Bytes(Vec<u8>),
    Shared { common: String },
}

#[test]
fn all_v4_conformance_cases() {
    let fixtures: Fixtures =
        serde_json::from_str(include_str!("fixtures/agent-plugin-cases.json")).unwrap();
    assert_eq!(fixtures.cases.len(), 158);
    let directory = tempfile::tempdir().unwrap();
    for (name, case) in fixtures.cases {
        let root = directory.path().join(&name);
        for path in case.dirs {
            fs::create_dir_all(root.join(path)).unwrap();
        }
        for (path, content) in case.files {
            let bytes = match &content {
                Contents::Text(text) => text.as_bytes(),
                Contents::Bytes(bytes) => bytes,
                Contents::Shared { common } => fixtures.common_files[common].as_bytes(),
            };
            let path = root.join(path);
            create_parent(&path);
            fs::write(path, bytes).unwrap();
        }
        for (path, target) in case.links {
            let path = root.join(path);
            create_parent(&path);
            symlink(target, path).unwrap();
        }
        let actual = load_package(&root.join("plugin"), Some(&root.join("data")));
        let (skills, servers) = actual.package.as_ref().map_or_else(
            || (vec![], vec![]),
            |pkg| {
                let skills = pkg
                    .skills
                    .iter()
                    .map(|skill| json!({"name": skill.name, "path": skill.path}))
                    .collect();
                (
                    skills,
                    serde_json::to_value(&pkg.servers)
                        .unwrap()
                        .as_array()
                        .unwrap()
                        .clone(),
                )
            },
        );
        let mut diagnostics = serde_json::to_value(&actual.diagnostics).unwrap();
        for diagnostic in diagnostics.as_array_mut().unwrap() {
            diagnostic.as_object_mut().unwrap().remove("message");
        }
        assert_eq!(
            json!({
                "accepted": actual.accepted(),
                "skills": skills,
                "servers": servers,
                "diagnostics": diagnostics,
            }),
            case.expected,
            "V4 case: {name}"
        );
    }
}

fn create_parent(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
}
