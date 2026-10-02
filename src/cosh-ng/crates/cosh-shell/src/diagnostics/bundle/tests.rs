use super::*;

#[test]
fn redacts_representative_secrets_and_private_keys() {
    let input = "token=abc123 password: hunter2 ghp_abcdefghijklmnopqrstuvwxyz\n\
                 authorization: \"Bearer abc\"\nAuthorization: Bearer unquoted-secret\n\
                 {\"key\":\"sk-abcdefghijklmnopqrstuvwxyz\"}\n\
                 cosh --token \"cli secret with spaces\"\n\
                 url=https://example.test/?access_token=query-secret\n\
                 mirrors=https://user:basic-password@example.test Bearer first-secret Bearer second-secret\n\
                 -----BEGIN PRIVATE KEY-----\nsecret-body\n-----END PRIVATE KEY-----\nkeep=true";
    let output = redact(input);
    for secret in [
        "abc123",
        "hunter2",
        "ghp_abcdefghijklmnopqrstuvwxyz",
        "Bearer abc",
        "unquoted-secret",
        "sk-abcdefghijklmnopqrstuvwxyz",
        "cli secret with spaces",
        "query-secret",
        "user:basic-password",
        "first-secret",
        "second-secret",
        "secret-body",
    ] {
        assert!(!output.contains(secret), "secret leaked: {secret}");
    }
    assert!(output.contains("keep=true"));
}

#[test]
fn collection_reports_when_the_file_limit_is_reached() {
    let directory = test_directory("file-limit");
    fs::create_dir_all(&directory).unwrap_or_else(|error| panic!("mkdir failed: {error}"));
    for index in 0..=MAX_FILES_PER_SOURCE {
        fs::write(
            directory.join(format!("event-{index}.log")),
            index.to_string(),
        )
        .unwrap_or_else(|error| panic!("write fixture failed: {error}"));
    }

    let (files, errors) =
        collect_named_files(Some(directory.clone()), Duration::from_secs(3600), |_| true);
    assert_eq!(files.len(), MAX_FILES_PER_SOURCE);
    assert!(errors.iter().any(|error| error.contains("limit reached")));
    cleanup(&directory);
}

#[cfg(unix)]
#[test]
fn collection_does_not_follow_file_or_directory_symlinks() {
    use std::os::unix::fs::symlink;

    let directory = test_directory("symlink");
    let root = directory.join("root");
    let outside = directory.join("outside");
    fs::create_dir_all(&root).unwrap_or_else(|error| panic!("mkdir root failed: {error}"));
    fs::create_dir_all(&outside).unwrap_or_else(|error| panic!("mkdir outside failed: {error}"));
    fs::write(outside.join("secret.log"), "must-not-be-collected")
        .unwrap_or_else(|error| panic!("write outside file failed: {error}"));
    symlink(outside.join("secret.log"), root.join("linked-file.log"))
        .unwrap_or_else(|error| panic!("symlink file failed: {error}"));
    symlink(&outside, root.join("linked-directory"))
        .unwrap_or_else(|error| panic!("symlink directory failed: {error}"));

    let (files, errors) = collect_named_files(Some(root), Duration::from_secs(3600), |_| true);
    assert!(files.is_empty());
    assert!(errors.is_empty());
    cleanup(&directory);
}

#[test]
fn parser_accepts_output_and_time_window() {
    let options = parse_args(&[
        "export".to_string(),
        "--output".to_string(),
        "bundle.json".to_string(),
        "--since-hours".to_string(),
        "6".to_string(),
    ])
    .unwrap_or_else(|error| panic!("parse failed: {error}"));
    assert_eq!(options.output, PathBuf::from("bundle.json"));
    assert_eq!(options.since, Duration::from_secs(6 * 3600));
}

#[test]
fn help_succeeds_and_excessive_time_window_is_rejected() {
    assert_eq!(run_cli(&["--help".to_string()]), 0);
    assert_eq!(run_cli(&["export".to_string(), "--help".to_string()]), 0);
    let error = parse_args(&[
        "export".to_string(),
        "--since-hours".to_string(),
        u64::MAX.to_string(),
    ])
    .expect_err("overflowing time window must fail");
    assert_eq!(error, "--since-hours is too large");
}

#[test]
fn truncated_sources_keep_the_recent_tail() {
    let directory = test_directory("tail");
    fs::create_dir_all(&directory).unwrap_or_else(|error| panic!("mkdir failed: {error}"));
    let path = directory.join("large.log");
    let mut content = b"old-head-must-not-survive\n".to_vec();
    content.resize(MAX_SOURCE_BYTES as usize + 64, b'x');
    content.extend_from_slice(b"\nrecent-tail-must-survive\n");
    fs::write(&path, content).unwrap_or_else(|error| panic!("write failed: {error}"));

    let (files, errors) =
        collect_named_files(Some(directory.clone()), Duration::from_secs(3600), |_| true);
    assert!(errors.is_empty(), "{errors:?}");
    let collected = files.into_iter().next().expect("collect large source");
    assert!(collected.truncated);
    assert!(!collected.content.contains("old-head-must-not-survive"));
    assert!(collected.content.contains("recent-tail-must-survive"));
    cleanup(&directory);
}

#[cfg(unix)]
#[test]
fn writes_bundle_with_private_permissions_and_refuses_overwrite() {
    use std::os::unix::fs::PermissionsExt;

    let directory = test_directory("permissions");
    fs::create_dir_all(&directory).unwrap_or_else(|error| panic!("mkdir failed: {error}"));
    let path = directory.join("bundle.json");
    atomic_private_write(&path, b"safe").unwrap_or_else(|error| panic!("write failed: {error}"));
    let mode = fs::metadata(&path)
        .unwrap_or_else(|error| panic!("metadata failed: {error}"))
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    assert!(atomic_private_write(&path, b"overwrite").is_err());
    cleanup(&directory);
}

#[test]
fn temporary_file_collision_preserves_existing_file() {
    let directory = test_directory("temporary-collision");
    fs::create_dir_all(&directory).unwrap_or_else(|error| panic!("mkdir failed: {error}"));
    let nonce = 42;
    let existing = directory.join(format!(".bundle.{}.{}.tmp", std::process::id(), nonce));
    fs::write(&existing, "must-survive")
        .unwrap_or_else(|error| panic!("write collision fixture failed: {error}"));

    let (temporary, file) = create_private_temp_with_nonce(&directory, "bundle", nonce)
        .unwrap_or_else(|error| panic!("create temporary file failed: {error}"));
    drop(file);

    assert_ne!(temporary, existing);
    assert_eq!(
        fs::read_to_string(&existing)
            .unwrap_or_else(|error| panic!("read collision failed: {error}")),
        "must-survive"
    );
    fs::remove_file(&temporary).unwrap_or_else(|error| panic!("remove temporary failed: {error}"));
    cleanup(&directory);
}

fn test_directory(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "cosh-diagnostic-{label}-test-{}-{}",
        std::process::id(),
        now_ms()
    ))
}

fn cleanup(path: &Path) {
    fs::remove_dir_all(path).unwrap_or_else(|error| panic!("cleanup failed: {error}"));
}
