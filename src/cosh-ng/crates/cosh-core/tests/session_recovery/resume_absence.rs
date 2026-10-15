use super::*;

fn scope_directories(root: &Path) -> Vec<PathBuf> {
    let mut scopes: Vec<_> = fs::read_dir(root)
        .expect("read session root")
        .map(|entry| entry.expect("read storage entry"))
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.len() == 24
                && name.chars().all(|character| character.is_ascii_hexdigit())
                && entry.file_type().expect("storage entry type").is_dir()
        })
        .map(|entry| entry.path())
        .collect();
    scopes.sort();
    scopes
}

#[test]
fn missing_resumes_across_workspaces_preserve_directories_and_location_index() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let root = temp.path().join("sessions");
    let seed_workspace = temp.path().join("seed-workspace");
    fs::create_dir_all(&seed_workspace).expect("seed workspace");
    configure(&home, &root);
    let seed = json_lines(&run_prompt(&home, &seed_workspace, &["seed"]));
    assert!(seed
        .iter()
        .any(|message| message["type"] == "result" && message["is_error"] == false));
    let index = home.join(
        ".copilot-shell/cosh-core/sessions/.budget/00000000-0000-4000-8000-000000000000.json",
    );
    let original_index = fs::read(&index).expect("seed location index");
    let original_locations: Value = serde_json::from_slice(&original_index).unwrap();
    let original_count = original_locations["locations"].as_array().unwrap().len();
    let original_scopes = scope_directories(&root);
    assert_eq!(original_count, 1);
    assert_eq!(original_scopes.len(), 1);

    for attempt in 0..8 {
        let workspace = temp.path().join(format!("missing-workspace-{attempt}"));
        fs::create_dir_all(&workspace).expect("distinct workspace");
        let id = format!("{attempt:08x}-2222-4222-8222-222222222222");
        let messages = json_lines(&run_prompt(
            &home,
            &workspace,
            &["--resume", &id, "missing"],
        ));
        assert!(messages
            .iter()
            .any(|message| message["session_error_code"] == "not_found"));
        let current_index = fs::read(&index).expect("unchanged location index");
        let current_locations: Value = serde_json::from_slice(&current_index).unwrap();
        assert_eq!(
            current_locations["locations"].as_array().unwrap().len(),
            original_count,
            "missing resume {attempt} registered a workspace"
        );
        assert_eq!(
            current_index, original_index,
            "missing resume {attempt} changed index bytes"
        );
        assert_eq!(
            scope_directories(&root),
            original_scopes,
            "missing resume {attempt} left a workspace directory"
        );
    }
}
