//! Canonical validation shares presentation while legacy output remains unchanged.

use super::{output::invoke, *};

fn validate(fixture: &Fixture) -> Command {
    let mut command = fixture.command();
    command
        .args(["config", "validate", "--config"])
        .arg(&fixture.config);
    command
}

#[test]
fn canonical_validation_reports_success_and_scope_without_runtime_work() {
    let f = Fixture::new();
    success(f.install().output().unwrap());
    success(f.init().output().unwrap());
    let bytes = fs::read(&f.config).unwrap();
    let inode = fs::metadata(&f.config).unwrap().ino();
    let current = invoke(&validate(&f));
    let legacy = invoke(f.command().args(["validate", "--config"]).arg(&f.config));
    assert!(current.status.success() && legacy.status.success());
    assert_eq!(current.stdout, legacy.stdout);
    assert_eq!(current.stdout, b"configuration valid\n");
    for against in [false, true] {
        let mut command = validate(&f);
        command.arg("--json");
        if against {
            command.arg("--against").arg(&f.config);
        }
        let output = invoke(&command);
        assert!(output.status.success() && output.stderr.is_empty());
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["command"], "config validate");
        assert_eq!(report["status"], "succeeded");
        assert_eq!(
            report["result"],
            json!({"kind":"configuration_checked", "policy_update_scope":against})
        );
        assert!(report.get("mutation").is_none());
    }
    assert_eq!(fs::read(&f.config).unwrap(), bytes);
    assert_eq!(fs::metadata(&f.config).unwrap().ino(), inode);
    assert!(!f.state.exists());
}

#[test]
fn validation_failures_mask_private_fields_and_keep_the_source() {
    let f = Fixture::new();
    fs::write(&f.config, "private-api-key: private-api-key").unwrap();
    let before = fs::read(&f.config).unwrap();
    let output = invoke(validate(&f).arg("--json"));
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "failed");
    assert_eq!(report["error"]["code"], "invalid_configuration");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-api-key"));
    assert_eq!(fs::read(&f.config).unwrap(), before);
    let output = invoke(
        f.command()
            .args(["config", "validate", "--config", "/missing", "--json"]),
    );
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["error"]["code"], "io");
    assert!(report.get("result").is_none());
}

#[test]
fn validation_human_options_and_usage_preserve_local_output_boundaries() {
    let f = Fixture::new();
    success(f.install().output().unwrap());
    success(f.init().output().unwrap());
    let output = invoke(validate(&f).arg("--quiet"));
    assert!(output.status.success());
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
    let output = invoke(validate(&f).arg("--verbose"));
    assert!(output.status.success());
    assert_eq!(output.stdout, b"configuration valid\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("no runtime activation"));
    for arguments in [
        vec!["config", "validate", "--json"],
        vec![
            "config", "validate", "--config", "/missing", "--json", "--quiet",
        ],
        vec!["validate", "--config", "/missing", "--json"],
    ] {
        let output = invoke(f.command().args(arguments));
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty() && !output.stderr.is_empty());
    }
}

#[test]
fn validation_rejects_nonregular_and_oversized_documents_without_blocking() {
    let f = Fixture::new();
    let fifo = std::ffi::CString::new(f.config.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: the C string is valid and this creates only the owned fixture FIFO.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let output = invoke(validate(&f).arg("--json"));
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["error"]["code"],
        "io"
    );
    fs::remove_file(&f.config).unwrap();
    fs::write(&f.config, vec![b' '; aw_config::MAX_DOCUMENT_BYTES + 1]).unwrap();
    let output = invoke(validate(&f).arg("--json"));
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["error"]["code"],
        "invalid_configuration"
    );
}
