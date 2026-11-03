use super::activity::{scope_directory, RuntimeProcess};
use super::*;
use std::time::Duration;

#[test]
fn rejected_fresh_processes_leave_no_new_controls() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let root = temp.path().join("sessions");
    fs::create_dir_all(&workspace).unwrap();
    configure(&home, &root);
    let seed = json_lines(&run_prompt(&home, &workspace, &["seed"]));
    let id = seed.iter().find(|line| line["type"] == "result").unwrap()["session_id"]
        .as_str()
        .unwrap();
    let scope = scope_directory(&root);
    let template: Value =
        serde_json::from_slice(&fs::read(scope.join(format!("{id}.json"))).unwrap()).unwrap();
    let mut leases = Vec::new();
    for index in 0..4 {
        let id = format!("{index:08x}-9999-4999-8999-999999999999");
        let mut envelope = template.clone();
        envelope["session_id"] = json!(id);
        envelope["padding"] = json!("");
        let overhead = serde_json::to_vec(&envelope).unwrap().len();
        envelope["padding"] = json!("x".repeat(25 * 1024 * 1024 - overhead));
        fs::write(
            scope.join(format!("{id}.json")),
            serde_json::to_vec(&envelope).unwrap(),
        )
        .unwrap();
        let lease = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(scope.join(format!(".{id}.live")))
            .unwrap();
        flock(&lease, FlockOperation::LockExclusive).unwrap();
        leases.push(lease);
    }
    let mut active = RuntimeProcess::start(&home, &workspace, &[]);
    for _ in 0..4 {
        let output = json_lines(&run_prompt(&home, &workspace, &["refused"]));
        let result = output.iter().find(|line| line["type"] == "result").unwrap();
        assert_eq!(result["session_error_code"], "budget_exhausted");
        let id = result["session_id"].as_str().unwrap();
        assert!(!scope.join(format!("{id}.json")).exists());
        assert!(
            !scope.join(format!(".{id}.lock")).exists(),
            "rejected first commit leaked generation control"
        );
        assert!(
            !scope.join(format!(".{id}.live")).exists(),
            "rejected first commit leaked activity control"
        );
    }
    let refused = active.turn("first refused turn");
    assert_eq!(refused["session_error_code"], "budget_exhausted");
    let id = refused["session_id"].as_str().unwrap();
    assert!(!scope.join(format!(".{id}.lock")).exists());
    assert!(!scope.join(format!(".{id}.live")).exists());
    for index in 0..4 {
        let id = format!("{index:08x}-9999-4999-8999-999999999999");
        assert_eq!(
            fs::metadata(scope.join(format!("{id}.json")))
                .unwrap()
                .len(),
            25 * 1024 * 1024
        );
    }
    drop(leases);
    for index in 0..4 {
        let id = format!("{index:08x}-9999-4999-8999-999999999999");
        fs::remove_file(scope.join(format!("{id}.json"))).unwrap();
    }
    let recovered = active.turn("retry after freeing space");
    assert_eq!(recovered["is_error"], false);
    assert_eq!(recovered["session_id"], refused["session_id"]);
    let stored: Value =
        serde_json::from_slice(&fs::read(scope.join(format!("{id}.json"))).unwrap()).unwrap();
    assert!(stored["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["content"] == "first refused turn"));
    let live = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(scope.join(format!(".{id}.live")))
        .expect("retry must reacquire the current activity inode");
    assert_eq!(
        flock(&live, FlockOperation::NonBlockingLockExclusive),
        Err(rustix::io::Errno::WOULDBLOCK),
        "retry must lease the current activity inode"
    );
}

#[test]
fn repeated_missing_resumes_leave_no_new_activity_control_files() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let root = temp.path().join("sessions");
    fs::create_dir_all(&workspace).unwrap();
    configure(&home, &root);
    for index in 0..8 {
        let id = format!("{index:08x}-2222-4222-8222-222222222222");
        let messages = json_lines(&run_prompt(
            &home,
            &workspace,
            &["--resume", &id, "missing"],
        ));
        assert!(messages
            .iter()
            .any(|message| message["session_error_code"] == "not_found"));
    }
    assert!(
        !root.exists(),
        "missing resumes must not create storage or activity controls"
    );
}

#[test]
fn missing_resume_preserves_preexisting_and_held_activity_files() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let root = temp.path().join("sessions");
    fs::create_dir_all(&workspace).unwrap();
    configure(&home, &root);
    json_lines(&run_prompt(&home, &workspace, &["seed"]));
    let id = "abcdefab-2222-4222-8222-222222222222";
    let path = scope_directory(&root).join(format!(".{id}.live"));
    fs::write(&path, b"preexisting").unwrap();
    let result = json_lines(&run_prompt(&home, &workspace, &["--resume", id, "missing"]));
    assert!(result
        .iter()
        .any(|message| message["session_error_code"] == "not_found"));
    assert_eq!(fs::read(&path).unwrap(), b"preexisting");
    let held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    flock(&held, FlockOperation::LockExclusive).unwrap();
    let contender = RuntimeProcess::start(&home, &workspace, &["--resume", id]);
    assert_eq!(
        contender.receive("result")["session_error_code"],
        "conflict"
    );
    assert_eq!(fs::read(&path).unwrap(), b"preexisting");
}

fn register_default_legacy(home: &Path, workspace: &Path) {
    configure_mock_provider(home);
    let output = json_lines(&run_prompt(
        home,
        workspace,
        &["register default legacy ownership"],
    ));
    let result = output.iter().find(|line| line["type"] == "result").unwrap();
    assert_eq!(result["is_error"], false);
    let clear = session_control(
        home,
        json!({"action":"clear", "workspace_scope":workspace,
        "session_ids":[result["session_id"]], "protected_session_ids":[]}),
    );
    assert_eq!(clear["ok"], true);
}

#[test]
fn relative_roots_share_the_user_budget_and_count_both_legacy_locations() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();
    configure(&home, Path::new("project-sessions"));
    let root = first.join("project-sessions");
    let mut guards = Vec::new();
    let mut legacy_bytes = 0;
    for (index, directory) in [first.join("sessions"), root.clone()]
        .into_iter()
        .enumerate()
    {
        fs::create_dir_all(&directory).unwrap();
        let id = format!("{index:08x}-3333-4333-8333-333333333333");
        let bytes = serde_json::to_vec(&vec![
            json!({"role":"user", "content":"x".repeat(1024 * 1024)}),
        ])
        .unwrap();
        legacy_bytes += bytes.len();
        let path = directory.join(format!("{id}.json"));
        fs::write(&path, bytes).unwrap();
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        flock(&file, FlockOperation::LockExclusive).unwrap();
        guards.push(file);
    }
    register_default_legacy(&home, &first);
    configure(&home, Path::new("project-sessions"));
    json_lines(&run_prompt(&home, &first, &["seed"]));
    let scope = scope_directory(&root);
    let seed = fs::read_dir(&scope)
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .unwrap()
        .path();
    let template: Value = serde_json::from_slice(&fs::read(&seed).unwrap()).unwrap();
    fs::remove_file(seed).unwrap();
    let filler_bytes = 100 * 1024 * 1024 - legacy_bytes - 64;
    for index in 0..4 {
        let id = format!("{index:08x}-4444-4444-8444-444444444444");
        let mut envelope = template.clone();
        envelope["session_id"] = json!(id);
        envelope["padding"] = json!("");
        let overhead = serde_json::to_vec(&envelope).unwrap().len();
        let size = filler_bytes / 4 + usize::from(index < filler_bytes % 4);
        envelope["padding"] = json!("x".repeat(size - overhead));
        fs::write(
            scope.join(format!("{id}.json")),
            serde_json::to_vec(&envelope).unwrap(),
        )
        .unwrap();
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(scope.join(format!(".{id}.live")))
            .unwrap();
        flock(&file, FlockOperation::LockExclusive).unwrap();
        guards.push(file);
    }
    let output = json_lines(&run_prompt(&home, &second, &["another workspace"]));
    let result = output
        .iter()
        .find(|message| message["type"] == "result")
        .unwrap();
    assert_eq!(result["session_error_code"], "budget_exhausted", "{result}");
    let id = result["session_id"].as_str().unwrap();
    assert!(!scope_directory(&second.join("project-sessions"))
        .join(format!("{id}.json"))
        .exists());
}

#[cfg(unix)]
#[test]
fn relative_roots_age_workspace_and_flat_legacy_arrays() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();
    fs::create_dir_all(first.join("sessions")).unwrap();
    register_default_legacy(&home, &first);
    configure(&home, Path::new("project-sessions"));
    json_lines(&run_prompt(&home, &first, &["register workspace"]));
    let mut files = Vec::new();
    for (index, directory) in [first.join("sessions"), first.join("project-sessions")]
        .into_iter()
        .enumerate()
    {
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("{index:08x}-5555-4555-8555-555555555555.json"));
        fs::write(
            &path,
            serde_json::to_vec(&vec![json!({"role":"user", "content":"expired legacy"})]).unwrap(),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::File::open(&path)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
            )
            .unwrap();
        files.push(path);
    }
    json_lines(&run_prompt(&home, &second, &["sweep registered legacy"]));
    assert!(
        files.iter().all(|path| !path.exists()),
        "both supported legacy locations must age out"
    );
    let scope = scope_directory(&first.join("project-sessions"));
    for path in &files {
        let id = path.file_stem().unwrap().to_str().unwrap();
        assert!(!scope.join(format!(".{id}.live")).exists());
        assert!(!scope.join(format!(".{id}.lock")).exists());
    }
}

#[test]
fn clear_and_commit_share_the_snapshot_lock_and_freed_space_is_retried() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let root = temp.path().join("sessions");
    fs::create_dir_all(&workspace).unwrap();
    configure(&home, &root);
    let output = json_lines(&run_prompt(&home, &workspace, &["clear target"]));
    let id = output.iter().find(|line| line["type"] == "result").unwrap()["session_id"]
        .as_str()
        .unwrap();
    let scope = scope_directory(&root);
    let mut template: Value =
        serde_json::from_slice(&fs::read(scope.join(format!("{id}.json"))).unwrap()).unwrap();
    let mut leases = Vec::new();
    let target_total = 100 * 1024 * 1024 - 64;
    for index in 0..4 {
        let current_id = if index == 0 {
            id.to_string()
        } else {
            format!("{index:08x}-6666-4666-8666-666666666666")
        };
        template["session_id"] = json!(current_id);
        template["padding"] = json!("");
        let overhead = serde_json::to_vec(&template).unwrap().len();
        let bytes = target_total / 4 + usize::from(index < target_total % 4);
        template["padding"] = json!("x".repeat(bytes - overhead));
        fs::write(
            scope.join(format!("{current_id}.json")),
            serde_json::to_vec(&template).unwrap(),
        )
        .unwrap();
        let lease = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(scope.join(format!(".{current_id}.live")))
            .unwrap();
        flock(&lease, FlockOperation::LockExclusive).unwrap();
        leases.push(lease);
    }
    let global = home.join(".copilot-shell/cosh-core/sessions/.budget/.budget.lock");
    let lock_path = if global.exists() {
        global
    } else {
        root.join(".budget.lock")
    };
    let budget = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
        .unwrap();
    flock(&budget, FlockOperation::LockExclusive).unwrap();
    let mut writer = RuntimeProcess::start(&home, &workspace, &[]);
    writer.receive("system");
    let mut child = isolated_command(&home)
        .arg("--session-control")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(serde_json::to_string(&json!({"action":"clear", "workspace_scope":workspace, "session_ids":[id], "protected_session_ids":[]})).unwrap().as_bytes()).unwrap();
    let clearer = RuntimeProcess::from_child(child);
    writeln!(
        writer.child.stdin.as_mut().unwrap(),
        "{}",
        json!({"type":"user", "message":{"role":"user", "content":"commit during clear"}})
    )
    .unwrap();
    assert!(
        clearer
            .lines
            .recv_timeout(Duration::from_millis(500))
            .is_err(),
        "clear bypassed the budget snapshot lock"
    );
    assert!(scope.join(format!("{id}.json")).exists());
    flock(&budget, FlockOperation::Unlock).unwrap();
    let cleared = clearer.lines.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(cleared["ok"], true, "{cleared}");
    assert!(!scope.join(format!("{id}.json")).exists());
    let first = writer.receive("result");
    assert_ne!(first["session_error_code"], "io", "{first}");
    assert!(first["is_error"] == false || first["session_error_code"] == "budget_exhausted");
    assert_eq!(
        writer.turn("space really freed after clear")["is_error"],
        false
    );
    assert!(!scope.join(format!("{id}.json")).exists());
}

#[test]
fn corrupt_legacy_with_a_clock_remains_accounted_and_is_not_evicted() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();
    configure(&home, Path::new("project-sessions"));
    json_lines(&run_prompt(&home, &first, &["register"]));
    let scope = scope_directory(&first.join("project-sessions"));
    let mut paths = Vec::new();
    for index in 0..4 {
        let path = first
            .join("project-sessions")
            .join(format!("{index:08x}-7777-4777-8777-777777777777.json"));
        let mut value = json!({"updated_at_ms":1,"note":"corrupt legacy","padding":""});
        let overhead = serde_json::to_vec(&value).unwrap().len();
        value["padding"] = json!("x".repeat(25 * 1024 * 1024 - overhead));
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        paths.push(path);
    }
    let output = json_lines(&run_prompt(&home, &second, &["sweep"]));
    let result = output.iter().find(|line| line["type"] == "result").unwrap();
    assert_eq!(
        result["session_error_code"], "budget_exhausted",
        "corrupt legacy bytes cannot disappear from accounting"
    );
    for path in paths {
        assert_eq!(fs::metadata(&path).unwrap().len(), 25 * 1024 * 1024);
        let value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["note"], "corrupt legacy");
        let id = path.file_stem().unwrap().to_str().unwrap();
        assert!(!scope.join(format!(".{id}.live")).exists());
        assert!(!scope.join(format!(".{id}.lock")).exists());
    }
}

#[cfg(unix)]
#[test]
fn active_relative_legacy_survives_other_workspace_sweep_until_exit() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    fs::create_dir_all(first.join("project-sessions")).unwrap();
    fs::create_dir_all(&second).unwrap();
    configure_auto_persist(&home, Path::new("project-sessions"), false);
    let id = "abcdefab-8888-4888-8888-888888888888";
    let path = first.join("project-sessions").join(format!("{id}.json"));
    let bytes =
        serde_json::to_vec(&vec![json!({"role":"user", "content":"active legacy"})]).unwrap();
    fs::write(&path, &bytes).unwrap();
    fs::File::open(&path)
        .unwrap()
        .set_times(
            fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
        )
        .unwrap();
    let mut active = RuntimeProcess::start(&home, &first, &["--resume", id]);
    assert_eq!(active.turn("keep running")["is_error"], false);
    configure(&home, Path::new("project-sessions"));
    json_lines(&run_prompt(&home, &second, &["sweep active legacy"]));
    assert_eq!(fs::read(&path).unwrap(), bytes);
    drop(active);
    json_lines(&run_prompt(&home, &second, &["sweep exited legacy"]));
    assert!(!path.exists());
}

#[cfg(unix)]
#[test]
fn custom_roots_preserve_unowned_workspace_message_arrays() {
    use std::os::unix::fs::MetadataExt;
    for relative in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let workspace = temp.path().join("workspace");
        let legacy = workspace.join("sessions");
        fs::create_dir_all(&legacy).unwrap();
        let root = if relative {
            PathBuf::from("project-sessions")
        } else {
            temp.path().join("custom-root")
        };
        configure(&home, &root);
        let path = legacy.join("abcdefab-8888-4888-8888-888888888888.json");
        let bytes = serde_json::to_vec(&vec![
            json!({"role":"user","content":"unrelated workspace history"}),
        ])
        .unwrap();
        fs::write(&path, &bytes).unwrap();
        fs::File::open(&path)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
            )
            .unwrap();
        let metadata = fs::metadata(&path).unwrap();
        let output = json_lines(&run_prompt(&home, &workspace, &["custom-root sweep"]));
        let result = output.iter().find(|line| line["type"] == "result").unwrap();
        assert_eq!(result["is_error"], false);
        assert!(
            path.exists(),
            "custom root must not age unowned workspace arrays"
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
        let after = fs::metadata(&path).unwrap();
        assert_eq!(
            (after.ino(), after.mode(), after.mtime(), after.mtime_nsec()),
            (
                metadata.ino(),
                metadata.mode(),
                metadata.mtime(),
                metadata.mtime_nsec()
            )
        );
        assert_eq!(fs::read_dir(&legacy).unwrap().count(), 1);
    }
}

#[test]
fn unowned_workspace_arrays_do_not_consume_custom_root_budget() {
    for relative in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let workspace = temp.path().join("workspace");
        let legacy = workspace.join("sessions");
        fs::create_dir_all(&legacy).unwrap();
        let root = if relative {
            PathBuf::from("project-sessions")
        } else {
            temp.path().join("custom-root")
        };
        configure(&home, &root);
        let mut guards = Vec::new();
        for index in 0..4 {
            let path = legacy.join(format!("{index:08x}-8888-4888-8888-888888888888.json"));
            let mut messages = json!([{"role":"user","content":""}]);
            let overhead = serde_json::to_vec(&messages).unwrap().len();
            messages[0]["content"] = json!("x".repeat(25 * 1024 * 1024 - overhead));
            fs::write(&path, serde_json::to_vec(&messages).unwrap()).unwrap();
            let guard = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            flock(&guard, FlockOperation::LockExclusive).unwrap();
            guards.push(guard);
        }
        let output = json_lines(&run_prompt(
            &home,
            &workspace,
            &["unowned bytes are outside the budget"],
        ));
        let result = output.iter().find(|line| line["type"] == "result").unwrap();
        assert_eq!(
            result["is_error"], false,
            "unowned arrays must not exhaust the Cosh budget: {result}"
        );
        for index in 0..4 {
            let path = legacy.join(format!("{index:08x}-8888-4888-8888-888888888888.json"));
            assert_eq!(fs::metadata(path).unwrap().len(), 25 * 1024 * 1024);
        }
    }
}

#[test]
fn unversioned_location_markers_refuse_sweeps_without_deleting_data() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let root = temp.path().join("custom-root");
    let legacy = workspace.join("sessions");
    fs::create_dir_all(&legacy).unwrap();
    configure(&home, &root);
    let path = legacy.join("abcdefab-aaaa-4aaa-8aaa-aaaaaaaaaaaa.json");
    let bytes = serde_json::to_vec(&vec![
        json!({"role":"user","content":"unowned old history"}),
    ])
    .unwrap();
    fs::write(&path, &bytes).unwrap();
    fs::File::open(&path)
        .unwrap()
        .set_times(
            fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
        )
        .unwrap();
    let index = home.join(
        ".copilot-shell/cosh-core/sessions/.budget/00000000-0000-4000-8000-000000000000.json",
    );
    fs::create_dir_all(index.parent().unwrap()).unwrap();
    let location = json!({"root":root,"workspace":workspace,"legacy":[legacy]});
    for value in [
        json!([location.clone()]),
        json!({"schema_version":999,"locations":[location]}),
    ] {
        let old = serde_json::to_vec(&value).unwrap();
        fs::write(&index, &old).unwrap();
        let output = json_lines(&run_prompt(
            &home,
            &workspace,
            &["refuse unproven legacy marker"],
        ));
        let result = output.iter().find(|line| line["type"] == "result").unwrap();
        assert_eq!(
            result["session_error_code"], "io",
            "unproven legacy markers must not authorize deletion"
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(fs::read(&index).unwrap(), old);
    }
}

#[test]
fn historical_legacy_directory_recreation_does_not_authorize_cleanup() {
    use std::os::unix::fs::MetadataExt;
    for default in [true, false] {
        for switch_absolute in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join("home");
            let workspace = temp.path().join("workspace");
            let legacy = workspace.join(if default {
                "sessions"
            } else {
                "project-sessions"
            });
            fs::create_dir_all(&legacy).unwrap();
            if default {
                register_default_legacy(&home, &workspace);
            } else {
                configure(&home, Path::new("project-sessions"));
                let lines = json_lines(&run_prompt(
                    &home,
                    &workspace,
                    &["register relative directory"],
                ));
                assert_eq!(
                    lines.iter().find(|line| line["type"] == "result").unwrap()["is_error"],
                    false
                );
            }
            fs::remove_dir_all(&legacy).unwrap();
            fs::create_dir_all(&legacy).unwrap();
            let path = legacy.join("12345678-1234-4234-8234-123456789abc.json");
            let bytes = serde_json::to_vec(&vec![
                json!({"role":"user","content":"another tool owns this array"}),
            ])
            .unwrap();
            fs::write(&path, &bytes).unwrap();
            fs::File::open(&path)
                .unwrap()
                .set_times(
                    fs::FileTimes::new()
                        .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
                )
                .unwrap();
            let before = fs::metadata(&path).unwrap();
            if switch_absolute {
                configure(&home, &temp.path().join("absolute-store"));
            }
            let lines = json_lines(&run_prompt(
                &home,
                &workspace,
                &["commit after changing roots"],
            ));
            assert_eq!(
                lines.iter().find(|line| line["type"] == "result").unwrap()["is_error"],
                false
            );
            assert_eq!(
                fs::read(&path).ok().as_deref(),
                Some(bytes.as_slice()),
                "recreated historical directory must not grant deletion authority"
            );
            let after = fs::metadata(&path).unwrap();
            assert_eq!(
                (
                    before.ino(),
                    before.mode(),
                    before.mtime(),
                    before.mtime_nsec()
                ),
                (after.ino(), after.mode(), after.mtime(), after.mtime_nsec())
            );
        }
    }
}

#[test]
fn recreated_historical_arrays_do_not_consume_the_budget() {
    for default in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let workspace = temp.path().join("workspace");
        let legacy = workspace.join(if default {
            "sessions"
        } else {
            "project-sessions"
        });
        fs::create_dir_all(&legacy).unwrap();
        if default {
            register_default_legacy(&home, &workspace);
        } else {
            configure(&home, Path::new("project-sessions"));
            json_lines(&run_prompt(
                &home,
                &workspace,
                &["register old relative directory"],
            ));
        }
        fs::remove_dir_all(&legacy).unwrap();
        fs::create_dir_all(&legacy).unwrap();
        configure(&home, &temp.path().join("absolute-store"));
        let mut guards = Vec::new();
        for index in 0..4 {
            let path = legacy.join(format!("{index:08x}-8888-4888-8888-888888888888.json"));
            let mut messages = json!([{"role":"user","content":""}]);
            let overhead = serde_json::to_vec(&messages).unwrap().len();
            messages[0]["content"] = json!("x".repeat(25 * 1024 * 1024 - overhead));
            fs::write(&path, serde_json::to_vec(&messages).unwrap()).unwrap();
            let guard = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            flock(&guard, FlockOperation::LockExclusive).unwrap();
            guards.push(guard);
        }
        let output = json_lines(&run_prompt(
            &home,
            &workspace,
            &["unowned bytes are outside the budget"],
        ));
        let result = output.iter().find(|line| line["type"] == "result").unwrap();
        assert_eq!(
            result["is_error"], false,
            "recreated historical arrays must not exhaust the Cosh budget: {result}"
        );
        for index in 0..4 {
            let path = legacy.join(format!("{index:08x}-8888-4888-8888-888888888888.json"));
            assert_eq!(fs::metadata(path).unwrap().len(), 25 * 1024 * 1024);
        }
    }
}

#[test]
fn lost_legacy_deletion_markers_keep_same_directory_bytes_in_the_user_budget() {
    use std::os::unix::fs::PermissionsExt;
    for default in [false, true] {
        for marker_change in 0..3 {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join("home");
            let workspace = temp.path().join("workspace");
            let root = workspace.join(if default {
                "sessions"
            } else {
                "project-sessions"
            });
            fs::create_dir_all(&root).unwrap();
            if default {
                register_default_legacy(&home, &workspace);
            } else {
                configure(&home, Path::new("project-sessions"));
                let lines = json_lines(&run_prompt(
                    &home,
                    &workspace,
                    &["register owned legacy directory"],
                ));
                let result = lines.iter().find(|line| line["type"] == "result").unwrap();
                assert_eq!(result["is_error"], false);
                assert_eq!(
                    session_control(
                        &home,
                        json!({"action":"clear","workspace_scope":workspace,"session_ids":[result["session_id"]],"protected_session_ids":[]})
                    )["ok"],
                    true
                );
            }
            let mut paths = Vec::new();
            for index in 0..4 {
                let path = root.join(format!("{index:08x}-baba-4aba-8aba-babababababa.json"));
                let mut data = json!([{"role":"user","content":""}]);
                let overhead = serde_json::to_vec(&data).unwrap().len();
                data[0]["content"] = json!("x".repeat(25 * 1024 * 1024 - overhead));
                fs::write(&path, serde_json::to_vec(&data).unwrap()).unwrap();
                paths.push(path);
            }
            let marker = root.join(".cosh-legacy-owner");
            match marker_change {
                0 => fs::remove_file(&marker).unwrap(),
                1 => fs::set_permissions(&marker, fs::Permissions::from_mode(0o644)).unwrap(),
                _ => fs::write(&marker, "abcdefab-cdef-4def-8def-abcdefabcdef").unwrap(),
            }
            configure(&home, &temp.path().join("absolute-store"));
            let lines = json_lines(&run_prompt(
                &home,
                &workspace,
                &["owned bytes still occupy capacity"],
            ));
            let result = lines.iter().find(|line| line["type"] == "result").unwrap();
            assert_eq!(
                result["session_error_code"], "budget_exhausted",
                "lost deletion proof must not make owned bytes disappear from the quota: {result}"
            );
            for path in paths {
                assert_eq!(fs::metadata(path).unwrap().len(), 25 * 1024 * 1024);
            }
        }
    }
}

#[test]
fn stale_workspace_without_legacy_ownership_does_not_block_another_workspace() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    let other = temp.path().join("other");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();
    fs::create_dir_all(&other).unwrap();
    configure(&home, &temp.path().join("store"));
    let seed = json_lines(&run_prompt(
        &home,
        &first,
        &["register modern-only workspace"],
    ));
    assert_eq!(
        seed.iter().find(|line| line["type"] == "result").unwrap()["is_error"],
        false
    );
    fs::rename(&first, temp.path().join("old-first")).unwrap();
    std::os::unix::fs::symlink(&other, &first).unwrap();
    let output = json_lines(&run_prompt(
        &home,
        &second,
        &["modern-only stale workspace is irrelevant"],
    ));
    let result = output.iter().find(|line| line["type"] == "result").unwrap();
    assert_eq!(
        result["is_error"], false,
        "no-legacy historical workspace must not block persistence: {result}"
    );
}
