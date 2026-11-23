//! Real CLI proof of daemon-independent generation and private, non-replacing files.

#![cfg(target_os = "linux")]

mod common;

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::process::Output;

use asc_cli::{Cli, InputError, Plan};

fn directory() -> tempfile::TempDir {
    // The store deliberately rejects /tmp's writable ancestor; use a private home parent.
    let home = fs::canonicalize(dirs::home_dir().expect("test user has a home directory")).unwrap();
    tempfile::tempdir_in(home).unwrap()
}

fn token_file(directory: &Path, token: &str) -> PathBuf {
    let path = directory.join("real token");
    fs::write(&path, token).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    path
}

fn arguments(real: &Path, output: &Path) -> Vec<OsString> {
    vec![
        "--socket".into(),
        "relative-unused-endpoint".into(),
        "key".into(),
        "generate-fake".into(),
        "--real-token-file".into(),
        real.into(),
        "--output".into(),
        output.into(),
    ]
}

fn assert_no_token_output(output: &Output, token: &str) {
    for stream in [&output.stdout, &output.stderr] {
        assert!(!String::from_utf8_lossy(stream).contains(token));
    }
}

#[test]
fn fake_generation_runs_locally_with_private_output_and_whole_string_difference() {
    let directory = directory();
    let real = format!("ghp_{}", "Ab12".repeat(16));
    let source = token_file(directory.path(), &format!("{real}\r\n"));
    for explicit_format in [false, true] {
        let output = directory.path().join(if explicit_format {
            "explicit"
        } else {
            "default"
        });
        let mut args = arguments(&source, &output);
        if explicit_format {
            args.extend(["--format".into(), "auto".into()]);
        }
        assert!(!args.iter().any(|arg| arg == real.as_str()));
        let cli =
            Cli::parse_from(std::iter::once(OsString::from("agent-sec-cli")).chain(args.clone()))
                .unwrap();
        assert!(matches!(cli.plan(), Plan::Local(_)));
        assert!(cli.socket().is_none());
        assert!(matches!(cli.request(), Err(InputError::LocalCommand)));
        let result = common::run(&args);
        assert_eq!(result.status.code(), Some(0));
        assert!(result.stdout.is_empty());
        assert!(result.stderr.is_empty());
        let fake = fs::read_to_string(&output).unwrap();
        assert_eq!(fake.len(), real.len());
        assert!(fake.starts_with("ghp_"));
        let different = real
            .bytes()
            .zip(fake.bytes())
            .filter(|(a, b)| a != b)
            .count();
        assert!(different * 5 >= real.len() * 4);
        asc_credential_store::validate_pair(&real, &fake).unwrap();
        assert_eq!(
            fs::metadata(&output).unwrap().permissions().mode() & 0o7777,
            0o600
        );
        assert_no_token_output(&result, &real);
        assert_no_token_output(&result, &fake);
        assert_eq!(fs::read_to_string(&source).unwrap(), format!("{real}\r\n"));
    }
}

#[test]
fn generation_rejects_unknown_shapes_similarity_and_unsafe_input_without_creating_output() {
    let directory = directory();
    for (real, format, mode) in [
        ("unknown.structured.token.value".to_owned(), "auto", 0o600),
        ("ghp_short".to_owned(), "auto", 0o600),
        (format!("github_pat_{}", "A".repeat(16)), "auto", 0o600),
        ("A".repeat(513), "auto", 0o600),
        ("Ab12".repeat(16), "auto", 0o644),
        ("Ab12".repeat(16), "unknown", 0o600),
        (format!("{}\ninternal", "Ab12".repeat(16)), "auto", 0o600),
    ] {
        let source = token_file(directory.path(), &real);
        fs::set_permissions(&source, fs::Permissions::from_mode(mode)).unwrap();
        let output = directory.path().join("not-created");
        let mut args = arguments(&source, &output);
        args.extend(["--format".into(), format.into()]);
        let result = common::run(&args);
        assert_eq!(result.status.code(), Some(1));
        assert!(result.stdout.is_empty());
        assert!(!result.stderr.is_empty());
        assert_no_token_output(&result, &real);
        assert!(!output.exists());
        assert_eq!(fs::read_to_string(&source).unwrap(), real);
    }
}

#[test]
fn output_never_overwrites_existing_files_or_symlinks_and_input_symlinks_are_rejected() {
    let directory = directory();
    let real = "Ab12".repeat(16);
    let source = token_file(directory.path(), &real);
    let target = directory.path().join("target");
    fs::write(&target, "unchanged").unwrap();
    let link = directory.path().join("output-link");
    symlink(&target, &link).unwrap();
    for output in [&source, &target, &link] {
        let result = common::run(&arguments(&source, output));
        assert_eq!(result.status.code(), Some(1));
        assert!(result.stdout.is_empty());
        assert_no_token_output(&result, &real);
        assert_eq!(fs::read_to_string(&target).unwrap(), "unchanged");
        assert_eq!(fs::read_to_string(&source).unwrap(), real);
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
    let input_link = directory.path().join("input-link");
    symlink(&source, &input_link).unwrap();
    let output = directory.path().join("not-created");
    let result = common::run(&arguments(&input_link, &output));
    assert_eq!(result.status.code(), Some(1));
    assert_no_token_output(&result, &real);
    assert!(!output.exists());
}

#[test]
fn local_file_paths_preserve_non_utf8_os_names() {
    let directory = directory();
    let source = directory
        .path()
        .join(OsString::from_vec(b"real-\xff".to_vec()));
    let output = directory
        .path()
        .join(OsString::from_vec(b"fake-\xfe".to_vec()));
    let real = "Ab12".repeat(16);
    fs::write(&source, &real).unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
    let result = common::run(&arguments(&source, &output));
    assert_eq!(result.status.code(), Some(0));
    assert!(result.stdout.is_empty());
    assert!(result.stderr.is_empty());
    let fake = fs::read_to_string(&output).unwrap();
    asc_credential_store::validate_pair(&real, &fake).unwrap();
    assert_no_token_output(&result, &real);
    assert_no_token_output(&result, &fake);
}
