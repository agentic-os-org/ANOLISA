use std::process::Command;

use tokenless_ccr::{SqliteStore, StashStore, extract_hash};
use tokenless_runtime::{CompressOptions, MIN_TOON_CHARS, compress_response_with_store};
use tokenless_stats::{
    CompressionMode, OperationType, StatsRecord, StatsRecorder, estimate_tokens,
    estimate_tokens_from_bytes, get_home_dir,
};

fn tokenless_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tokenless"))
}

struct TempStatsDb {
    dir: std::path::PathBuf,
    path: std::path::PathBuf,
    record_id: i64,
}

impl TempStatsDb {
    fn new() -> Option<Self> {
        let home = get_home_dir();
        if home.is_empty() {
            return None;
        }
        let unique = format!(
            ".tokenless-cli-integration-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_nanos()
        );
        let dir = std::path::PathBuf::from(home).join(unique);
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("stats.db");
        let recorder = StatsRecorder::new(&path).ok()?;
        let record_id = recorder
            .record(
                &StatsRecord::new(
                    OperationType::CompressResponse,
                    "integration-agent".to_string(),
                    17,
                    10,
                    9,
                    5,
                )
                .with_session_id("integration-session")
                .with_tool_use_id("integration-tool")
                .with_text("keep\nremove\n".to_string(), "keep\n".to_string()),
            )
            .ok()?;
        Some(Self {
            dir,
            path,
            record_id,
        })
    }

    fn command(&self) -> Command {
        let mut command = tokenless_bin();
        command.env("TOKENLESS_STATS_DB", &self.path);
        command
    }
}

impl Drop for TempStatsDb {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

struct TempDataDir {
    _temp_dir: tempfile::TempDir,
    root: std::path::PathBuf,
    data_dir: std::path::PathBuf,
}

impl TempDataDir {
    /// Use system-wide temporary storage so rejection tests also run when the
    /// default temporary directory is inside the trusted user profile.
    fn new_external() -> Option<Self> {
        #[cfg(windows)]
        let base = std::path::PathBuf::from(std::env::var_os("SystemRoot")?).join("Temp");
        #[cfg(unix)]
        let base = std::path::PathBuf::from("/tmp");
        let temp_dir = tempfile::Builder::new()
            .prefix("tokenless-external-data-dir-")
            .tempdir_in(&base)
            .inspect_err(|error| {
                eprintln!(
                    "external-root fixture unavailable at {}: {error}",
                    base.display()
                )
            })
            .ok()?;
        let root = temp_dir.path().canonicalize().ok()?;
        let home = get_home_dir();
        if !home.is_empty() && root.starts_with(std::path::Path::new(&home).canonicalize().ok()?) {
            eprintln!(
                "external-root fixture is inside the trusted home: {}",
                root.display()
            );
            return None;
        }
        let data_dir = root.join("databases");
        Some(Self {
            _temp_dir: temp_dir,
            root,
            data_dir,
        })
    }

    fn new() -> Option<Self> {
        let temp_dir = tempfile::Builder::new()
            .prefix("tokenless-data-dir-integration-")
            .tempdir()
            .ok()?;
        let root = temp_dir.path().to_path_buf();
        let data_dir = root.join("databases");
        Some(Self {
            _temp_dir: temp_dir,
            root,
            data_dir,
        })
    }

    fn command(&self) -> Command {
        let mut command = tokenless_bin();
        self.configure(&mut command);
        command
    }

    /// Apply this fixture's database isolation to a `Command` the caller
    /// builds itself — the unwritable-stderr tests need to set stdio and
    /// cannot go through [`TempDataDir::command`].
    fn configure<'a>(&self, command: &'a mut Command) -> &'a mut Command {
        command
            .env("TOKENLESS_DATA_DIR", &self.data_dir)
            .env_remove("TOKENLESS_STATS_DB")
            .env_remove("TOKENLESS_STASH_DB")
    }
}

#[test]
fn data_dir_env_routes_stats_and_stash_databases() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };

    let stats_output = fixture
        .command()
        .args(["stats", "summary"])
        .output()
        .unwrap();
    assert!(
        stats_output.status.success(),
        "stats command failed: {}",
        String::from_utf8_lossy(&stats_output.stderr)
    );
    assert!(!String::from_utf8_lossy(&stats_output.stderr).contains("ignoring TOKENLESS_DATA_DIR"));
    assert!(fixture.data_dir.join("stats.db").is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fixture.data_dir.metadata().unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    let stash_output = fixture
        .command()
        .args(["retrieve", "abcdef0123456789abcdef01"])
        .output()
        .unwrap();
    assert!(!stash_output.status.success());
    assert!(
        fixture.data_dir.join("stash.db").is_file(),
        "retrieve stderr: {}",
        String::from_utf8_lossy(&stash_output.stderr)
    );
}

#[test]
fn stats_db_env_takes_precedence_over_data_dir() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };
    let explicit_dir = fixture.data_dir.join("explicit");
    std::fs::create_dir_all(&explicit_dir).unwrap();
    let explicit_db = explicit_dir.join("stats.db");

    let output = fixture
        .command()
        .env("TOKENLESS_STATS_DB", &explicit_db)
        .args(["stats", "summary"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stats command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(explicit_db.is_file());
    assert!(!fixture.data_dir.join("stats.db").exists());
}

#[test]
fn stash_db_env_takes_precedence_over_data_dir() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };
    let explicit_dir = fixture.data_dir.join("explicit");
    std::fs::create_dir_all(&explicit_dir).unwrap();
    let explicit_db = explicit_dir.join("stash.db");

    let output = fixture
        .command()
        .env("TOKENLESS_STASH_DB", &explicit_db)
        .args(["retrieve", "abcdef0123456789abcdef01"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(explicit_db.is_file());
    assert!(!fixture.data_dir.join("stash.db").exists());
}

#[test]
fn invalid_explicit_data_dir_does_not_fall_back_to_home() {
    let output = tokenless_bin()
        .env("TOKENLESS_DATA_DIR", "relative/data")
        .env_remove("TOKENLESS_STATS_DB")
        .args(["stats", "summary"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("path 'relative/data' is not absolute"));
}

#[test]
fn invalid_explicit_data_dir_does_not_block_stats_status() {
    let output = tokenless_bin()
        .env("TOKENLESS_DATA_DIR", "relative/data")
        .env_remove("TOKENLESS_STATS_DB")
        .args(["stats", "status"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Stats recording:"));
    assert!(output.stderr.is_empty());
}

#[test]
fn valid_stats_db_override_wins_over_invalid_data_dir() {
    let fixture = match TempStatsDb::new() {
        Some(fixture) => fixture,
        None => return,
    };
    let output = fixture
        .command()
        .env("TOKENLESS_DATA_DIR", "relative/data")
        .args(["stats", "summary"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(fixture.path.is_file());
}

#[test]
fn stats_db_override_cannot_escape_selected_data_dir() {
    let fixture = match TempDataDir::new_external() {
        Some(fixture) => fixture,
        None => return,
    };
    let outside_db = fixture.root.join("outside-stats.db");
    let output = fixture
        .command()
        .env("TOKENLESS_STATS_DB", &outside_db)
        .args(["stats", "summary"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(!outside_db.exists());
    assert!(fixture.data_dir.join("stats.db").is_file());
    assert!(String::from_utf8_lossy(&output.stderr).contains("ignoring TOKENLESS_STATS_DB"));
}

#[test]
fn stats_db_override_allows_trusted_home_outside_selected_data_dir() {
    let Some(fixture) = TempStatsDb::new() else {
        return;
    };
    let data = tempfile::tempdir().unwrap();
    let outside_db = fixture.dir.join("home-stats.db");
    let output = tokenless_bin()
        .env("TOKENLESS_DATA_DIR", data.path())
        .env("TOKENLESS_STATS_DB", &outside_db)
        .args(["stats", "summary"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(outside_db.is_file());
    assert!(!data.path().join("stats.db").exists());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("ignoring TOKENLESS_STATS_DB"));
}

#[test]
fn compress_schema_from_stdin() {
    let schema = r#"{"function":{"name":"test","description":"A test function","parameters":{"type":"object","properties":{"x":{"type":"string","title":"Remove Me","examples":["ex1"]}}}}}"#;
    let output = tokenless_bin()
        .args(["compress-schema"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(schema.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(output.status.success(), "compress-schema should succeed");
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("output should be valid JSON");
    assert!(result["function"]["name"].is_string());
}

#[test]
fn compress_schema_from_file() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("schema.json");
    std::fs::write(
        &f,
        r#"{"function":{"name":"f","description":"desc","parameters":{"type":"object","properties":{}}}}"#,
    )
    .unwrap();
    let output = tokenless_bin()
        .args(["compress-schema", "--file", f.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["function"]["name"], "f");
}

/// Run a tokenless subcommand with `input` on stdin and capture its output.
/// Stats and SLS stay off so the assertions see only the emitted payload.
fn run_stdin_command(
    fixture: &TempDataDir,
    compression_enabled: &str,
    args: &[&str],
    input: &str,
) -> std::process::Output {
    fixture
        .command()
        .env("TOKENLESS_COMPRESSION_ENABLED", compression_enabled)
        .env("TOKENLESS_STATS_ENABLED", "0")
        .env("TOKENLESS_SLS_ENABLED", "0")
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(input.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap()
}

/// `compress-schema` shares the byte-exact stdout contract of `compress-toon`
/// and `retrieve`: the payload is emitted verbatim, so a passthrough run
/// reproduces the input byte-for-byte (no LF appended, none doubled) and an
/// encoded run emits exactly the candidate the stats recorder measures.
#[test]
fn compress_schema_stdout_is_byte_exact() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };
    let schema = r#"{"function":{"name":"test","description":"A test function","parameters":{"type":"object","properties":{"x":{"type":"string","title":"Remove Me","examples":["ex1"]}}}}}"#;

    // Dry-run passthrough emits the input unchanged; the trailing-LF variant
    // catches a doubled newline just as the bare variant catches an added one.
    for payload in [schema.to_string(), format!("{schema}\n")] {
        let output = run_stdin_command(&fixture, "0", &["compress-schema", "--no-stash"], &payload);
        assert!(
            output.status.success(),
            "compress-schema failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.stdout,
            payload.as_bytes(),
            "dry-run compress-schema must reproduce the input byte-for-byte"
        );
    }

    // With compression on, stdout is exactly the emitted candidate: compact
    // JSON from the schema compressor, with no LF appended.
    let long_desc = "Run a shell command in the workspace. ".repeat(20);
    let schema = format!(
        r#"{{"function":{{"name":"test","description":"{long_desc}","parameters":{{"type":"object","properties":{{"x":{{"type":"string","title":"Remove Me","examples":["ex1"]}}}}}}}}}}"#
    );
    let output = run_stdin_command(&fixture, "1", &["compress-schema", "--no-stash"], &schema);
    assert!(
        output.status.success(),
        "compress-schema failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_str(&schema).unwrap();
    let expected =
        serde_json::to_string(&tokenless_schema::SchemaCompressor::new().compress(&value)).unwrap();
    assert_ne!(
        expected, schema,
        "fixture must compress, otherwise the run is a passthrough"
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        expected,
        "compress-schema stdout must equal the emitted candidate byte-for-byte"
    );
}

#[test]
fn compress_schema_batch_mode() {
    let schemas = r#"[{"function":{"name":"a","parameters":{"type":"object","properties":{}}}},{"function":{"name":"b","parameters":{"type":"object","properties":{}}}}]"#;
    let output = tokenless_bin()
        .args(["compress-schema", "--batch"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(schemas.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(result.is_array());
}

#[test]
fn compress_schema_batch_gemini_function_declarations() {
    // copilot-shell BeforeModel hooks pass Gemini SDK tool entries
    // ({"functionDeclarations": [...]}). The batch path must compress the
    // nested declarations and keep the wrapper shape so the host can apply
    // the rewritten array unchanged.
    let long_desc = "Run a shell command in the workspace. ".repeat(20);
    let schemas = serde_json::json!([
        {
            "functionDeclarations": [
                {
                    "name": "shell",
                    "description": long_desc,
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "command": {"type": "string", "description": long_desc}
                        }
                    }
                }
            ]
        }
    ]);
    let input = serde_json::to_string(&schemas).unwrap();
    let output = tokenless_bin()
        .args(["compress-schema", "--batch"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(input.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(output.status.success(), "compress-schema should succeed");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let decls = &result[0]["functionDeclarations"];
    assert_eq!(decls[0]["name"], "shell");
    // Limits are char counts, not byte lengths (the stash marker carries a
    // multibyte ellipsis).
    assert!(decls[0]["description"].as_str().unwrap().chars().count() <= 256);
    let param_desc = decls[0]["parameters"]["properties"]["command"]["description"]
        .as_str()
        .unwrap();
    assert!(param_desc.chars().count() <= 160);
    assert!(
        output.stdout.len() < input.len(),
        "compressed output must be smaller than the input"
    );
}

#[test]
fn compress_schema_tools_request_container() {
    let input = serde_json::to_string_pretty(&serde_json::json!({
        "model": "example-model",
        "tools": [{
            "type": "function",
            "function": {
                "name": "lookup",
                "description": "A".repeat(2000),
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "B".repeat(1000)
                        }
                    }
                }
            }
        }]
    }))
    .unwrap();
    let output = tokenless_bin()
        .env("TOKENLESS_COMPRESSION_ENABLED", "1")
        .env("TOKENLESS_STATS_ENABLED", "0")
        .args(["compress-schema", "--no-stash"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(input.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();

    assert!(
        output.status.success(),
        "compress-schema failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["model"], "example-model");
    assert!(
        result["tools"][0]["function"]["description"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 256
    );
    assert!(
        result["tools"][0]["function"]["parameters"]["properties"]["query"]["description"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 160
    );
}

#[test]
fn compress_response_from_stdin() {
    let response =
        r#"{"data":"value","debug":"remove","trace":"remove","empty_field":"","null_field":null}"#;
    let output = tokenless_bin()
        .args(["compress-response"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(response.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result,
        serde_json::from_str::<serde_json::Value>(response).unwrap()
    );
}

#[test]
fn compress_response_cli_matches_runtime_library() {
    let response = serde_json::to_string(&serde_json::json!({
        "items": (0..100).collect::<Vec<_>>(),
        "debug": "remove",
        "empty": null,
    }))
    .unwrap();
    let expected = compress_response_with_store(
        &response,
        &CompressOptions {
            truncate_arrays_at: Some(4),
            stash_enabled: false,
            ..CompressOptions::default()
        },
        true,
        None,
    )
    .unwrap();

    let output = tokenless_bin()
        .env("TOKENLESS_COMPRESSION_ENABLED", "1")
        .env("TOKENLESS_STATS_ENABLED", "0")
        .env("TOKENLESS_SLS_ENABLED", "0")
        .args([
            "compress-response",
            "--truncate-arrays-at",
            "4",
            "--no-stash",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(response.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();

    assert!(output.status.success());
    // Exact equality with the runtime library output: no `trim_end()`, so an
    // appended trailing newline fails the comparison.
    assert_eq!(String::from_utf8(output.stdout).unwrap(), expected.output);
}

/// `compress-response` shares the byte-exact stdout contract of
/// `compress-toon` and `retrieve`: a passthrough run must reproduce the input
/// verbatim, so automation can detect the passthrough by comparing stdout with
/// the input payload. The runs here are dry-run with statistics off, so they
/// assert the emitted bytes only - equality with the recorded `after` text
/// holds in active mode alone (see the CLI reference).
#[test]
fn compress_response_stdout_is_byte_exact() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };
    let input = r#"{"data":"value","debug":"remove"}"#;
    // The trailing-LF variant catches a doubled newline just as the bare
    // variant catches an added one.
    for payload in [input.to_string(), format!("{input}\n")] {
        let output = run_stdin_command(
            &fixture,
            "0",
            &["compress-response", "--no-stash"],
            &payload,
        );
        assert!(
            output.status.success(),
            "compress-response failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.stdout,
            payload.as_bytes(),
            "dry-run compress-response must reproduce the input byte-for-byte"
        );
    }
}

#[test]
fn compress_response_stats_use_unicode_aware_estimates() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };
    let response = serde_json::to_string(&serde_json::json!({
        "tail": "世界".repeat(300)
    }))
    .unwrap();
    let output = fixture
        .command()
        .env("TOKENLESS_COMPRESSION_ENABLED", "1")
        .env("TOKENLESS_STATS_ENABLED", "1")
        .env("TOKENLESS_SLS_ENABLED", "0")
        .args([
            "compress-response",
            "--truncate-strings-at",
            "120",
            "--no-stash",
            "--agent-id",
            "integration-agent",
            "--session-id",
            "unicode-session",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(response.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(
        output.status.success(),
        "compress-response failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let compressed = String::from_utf8(output.stdout).unwrap();
    let recorder = StatsRecorder::new(fixture.data_dir.join("stats.db")).unwrap();
    let records = recorder
        .records_by_session("unicode-session", None)
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].before_tokens, estimate_tokens(&response));
    // The recorded `after` is measured on exactly the emitted bytes: no
    // compensating trim, so an LF the CLI appended but the recorder did not
    // measure fails here.
    assert_eq!(records[0].after_tokens, estimate_tokens(&compressed));
}

#[test]
fn dry_run_no_savings_keeps_the_no_savings_warning() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };
    let response = r#"{"value":1}"#;
    let output = fixture
        .command()
        .env("TOKENLESS_COMPRESSION_ENABLED", "0")
        .env("TOKENLESS_STATS_ENABLED", "0")
        .env("TOKENLESS_SLS_ENABLED", "0")
        .args(["compress-response", "--no-stash"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(response.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        response.as_bytes(),
        "dry-run passthrough must reproduce the input byte-for-byte"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("response compression did not reduce size"));
    assert!(stderr.contains("dry-run mode"));
}

#[test]
fn compress_response_from_file() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("response.json");
    std::fs::write(&f, r#"{"key":"value","logs":"remove me"}"#).unwrap();
    let output = tokenless_bin()
        .args(["compress-response", "--file", f.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(result.get("key").is_some());
}

#[test]
fn compress_response_array_tail_preserve_max_does_not_abort() {
    // Regression: `--array-tail-preserve` is an unconstrained usize. Even
    // usize::MAX must not overflow the head+tail budget (panic in debug,
    // abort in release); the saturated budget keeps the whole array.
    let huge = usize::MAX.to_string();
    let output = tokenless_bin()
        .args([
            "compress-response",
            "--truncate-arrays-at",
            "1",
            "--array-tail-preserve",
            huge.as_str(),
            "--no-stash",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(b"[1,2,3]\n")?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(
        output.status.success(),
        "CLI aborted on large --array-tail-preserve: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result, serde_json::json!([1, 2, 3]));
}

#[test]
fn compress_response_no_stash() {
    let response = r#"{"data":"value","debug":"remove"}"#;
    let output = tokenless_bin()
        .args(["compress-response", "--no-stash"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(response.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(output.status.success());
}

#[test]
fn no_stash_response_to_toon_roundtrip_is_decodable() {
    // Regression: `--no-stash` is a supported mode, and its plain truncation
    // marker used to lack a TOON quoting trigger, so the pipeline
    // compress-response --no-stash -> compress-toon -> decompress-toon exited
    // 2 at the mid-array marker while both compression commands reported
    // success. The marker now carries the `, not stashed` clause, which
    // forces TOON quoting, so the whole pipeline must round-trip and the
    // paired decoder must accept the TOON output.
    let bad: Vec<String> = (0..60)
        .map(|i| format!("result-{i}-{}", "x".repeat(80)))
        .collect();
    let good: Vec<serde_json::Value> = (0..5)
        .map(|i| serde_json::json!({ "identifier": i, "repeated_field_alpha": "alpha-value" }))
        .collect();
    let payload = serde_json::json!({
        "bad": bad,
        "good": good,
        "tool": "search",
        "status": "ok",
    });

    let run_stage = |args: &[&str], input: &[u8]| -> Vec<u8> {
        let output = tokenless_bin()
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child.stdin.take().unwrap().write_all(input)?;
                child.wait_with_output()
            })
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };

    let compressed = run_stage(
        &["compress-response", "--no-stash"],
        payload.to_string().as_bytes(),
    );
    let toon = run_stage(&["compress-toon"], &compressed);
    // The TOON candidate must actually be TOON (not a JSON fallback) for the
    // pipeline under test.
    let toon_text = String::from_utf8_lossy(&toon);
    assert!(
        toon_text.contains("more items truncated, not stashed"),
        "plain marker present in the TOON output"
    );
    let decompressed = run_stage(&["decompress-toon"], &toon);
    let decoded: serde_json::Value = serde_json::from_slice(&decompressed).unwrap();
    assert_eq!(decoded["tool"], "search", "root key survives the pipeline");
    assert_eq!(decoded["status"], "ok", "root key survives the pipeline");
    assert_eq!(
        decoded["bad"].as_array().map(Vec::len),
        Some(41),
        "32 head + marker + 8 tail preserved"
    );
    assert_eq!(decoded["good"].as_array().map(Vec::len), Some(5));
}

#[test]
fn stats_list_empty() {
    let output = tokenless_bin().args(["stats", "list"]).output().unwrap();
    // May succeed or fail depending on db state; should not panic
    let _ = output.status;
}

#[test]
fn stats_summary() {
    let output = tokenless_bin().args(["stats", "summary"]).output().unwrap();
    let _ = output.status;
}

#[test]
fn write_only_compression_reclaims_expired_stash_payloads() {
    use std::io::Write;
    use std::process::Stdio;

    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };
    let stash_db = fixture.data_dir.join("stash.db");
    let compress = |payload: &str| {
        let input = serde_json::json!({ "text": payload }).to_string();
        let mut child = fixture
            .command()
            .env("TOKENLESS_COMPRESSION_ENABLED", "1")
            .env("TOKENLESS_STATS_ENABLED", "0")
            .args(["compress-response", "--truncate-strings-at", "120"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "compression failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        extract_hash(&String::from_utf8_lossy(&output.stdout))
            .expect("compression must emit a recovery instruction")
            .to_string()
    };

    let old_key = compress(&format!("old session {}", "x".repeat(1024)));
    let conn = rusqlite::Connection::open(&stash_db).unwrap();
    conn.execute("UPDATE stash SET expires_at = 0", []).unwrap();
    drop(conn);

    let current_payload = format!("current session {}", "y".repeat(1024));
    let current_key = compress(&current_payload);

    // A retrieve would purge the old row and conceal this regression. Inspect
    // durable state after two separate compression processes instead.
    let conn = rusqlite::Connection::open(&stash_db).unwrap();
    let rows: usize = conn
        .query_row("SELECT COUNT(*) FROM stash", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        rows, 1,
        "write-only compression retained the expired payload"
    );
    let (retained_key, retained): (String, String) = conn
        .query_row("SELECT hash, payload FROM stash", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(retained_key, current_key);
    assert_eq!(retained, current_payload);
    let store = SqliteStore::new(&stash_db).unwrap();
    assert_eq!(store.retrieve(&current_key).unwrap(), Some(current_payload));
    assert_eq!(store.retrieve(&old_key).unwrap(), None);
}

#[test]
fn retrieve_missing_hash() {
    let output = tokenless_bin()
        .args(["retrieve", "000000000000000000000000"])
        .output()
        .unwrap();
    // Should fail gracefully (hash not found), not panic
    assert!(!output.status.success());
}

#[test]
fn retrieve_invalid_hash() {
    let output = tokenless_bin()
        .args(["retrieve", "not-a-valid-hash"])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn retrieve_stdout_is_byte_exact_without_extra_trailing_newline() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };
    let stash_db = fixture.data_dir.join("stash.db");
    // Long string forces reversible truncation + stash. The stored payload is
    // this string value (not the whole JSON document) and has no trailing `\n`.
    let stashed_string = format!("HELLO_RETRIEVE_EXACT_{}", "X".repeat(200));
    let original = format!("{{\"s\":\"{stashed_string}\"}}");

    let compressed = fixture
        .command()
        // Force compression on so a caller/home config with
        // TOKENLESS_COMPRESSION_ENABLED=0 (or compression_enabled:false)
        // cannot dry-run this subprocess and skip the stash marker.
        .env("TOKENLESS_COMPRESSION_ENABLED", "1")
        .env("TOKENLESS_STATS_ENABLED", "0")
        .args([
            "compress-response",
            "--truncate-strings-at",
            "120",
            "--stash-db",
        ])
        .arg(&stash_db)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(original.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(
        compressed.status.success(),
        "compress-response failed: {}",
        String::from_utf8_lossy(&compressed.stderr)
    );
    let compressed_text = String::from_utf8_lossy(&compressed.stdout);
    let marker = tokenless_ccr::extract_hash(&compressed_text)
        .expect("compressed output should contain a recovery instruction");

    let retrieved = fixture
        .command()
        .args(["retrieve", marker, "--stash-db"])
        .arg(&stash_db)
        .output()
        .unwrap();
    assert!(
        retrieved.status.success(),
        "retrieve failed: {}",
        String::from_utf8_lossy(&retrieved.stderr)
    );
    assert_eq!(
        retrieved.stdout.as_slice(),
        stashed_string.as_bytes(),
        "retrieve must restore the stashed payload byte-for-byte; \
         an extra trailing newline breaks end-to-end lossless recovery"
    );
}

#[test]
fn retrieve_records_events_and_summary_reports_attribution() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };
    let content = format!(
        r#"{{"records":[{}]}}"#,
        (0..200)
            .map(|i| format!(r#"{{"id":{i},"name":"row-{i}"}}"#))
            .collect::<Vec<_>>()
            .join(",")
    );
    let mut request: serde_json::Value = serde_json::from_str(&post_tool_request_json(
        &content,
        true,
        "retrieve-attribution",
    ))
    .unwrap();
    request["input"]["tool_name"] = serde_json::json!("Bash");
    request["input"]["content_origin"] = serde_json::json!("command_output");
    let output = spawn_with_stdin(
        fixture
            .command()
            .env("TOKENLESS_COMPRESSION_ENABLED", "1")
            .env("TOKENLESS_STATS_ENABLED", "1")
            .env("TOKENLESS_SLS_ENABLED", "0"),
        &["compress"],
        &request.to_string(),
    );
    assert!(output.status.success());
    let response: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    let marker_text = response["result"]["output"].as_str().unwrap();
    let marker = tokenless_ccr::extract_hash(marker_text)
        .expect("array truncation should emit a recovery instruction");

    let hit = fixture
        .command()
        .env("TOKENLESS_STATS_ENABLED", "1")
        .args(["retrieve", marker])
        .output()
        .unwrap();
    assert!(hit.status.success());
    let miss = fixture
        .command()
        .env("TOKENLESS_STATS_ENABLED", "1")
        .args(["retrieve", "000000000000000000000000"])
        .output()
        .unwrap();
    assert!(!miss.status.success());

    let recorder = StatsRecorder::new(fixture.data_dir.join("stats.db")).unwrap();
    let totals = recorder.retrieve_totals().unwrap();
    assert_eq!(totals.hits, 1);
    assert_eq!(totals.misses, 1);
    assert!(totals.retrieved_tokens > 0);

    let summary = fixture
        .command()
        .env("TOKENLESS_STATS_ENABLED", "1")
        .args(["stats", "summary"])
        .output()
        .unwrap();
    assert!(summary.status.success());
    let text = String::from_utf8_lossy(&summary.stdout);
    assert!(text.contains("Attribution:"));
    assert!(text.contains("Retrieves:      1 hits / 1 misses / 0 errors"));
}

#[test]
fn compress_response_no_savings_rolls_back_orphan_stash() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };
    let stash_db = fixture.data_dir.join("stash.db");
    // Small array + aggressive truncate: marker overhead makes after_tokens
    // >= before_tokens, so CLI falls back to the original input.
    let original = r#"["a","b"]"#;

    let output = fixture
        .command()
        .env("TOKENLESS_COMPRESSION_ENABLED", "1")
        .env("TOKENLESS_STATS_ENABLED", "0")
        .args([
            "compress-response",
            "--truncate-arrays-at",
            "1",
            "--stash-db",
        ])
        .arg(&stash_db)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(original.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(
        output.status.success(),
        "compress-response failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("did not reduce size"),
        "expected no-savings path, stderr={stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("tokenless:"),
        "no-savings stdout must not expose markers: {stdout}"
    );
    // Discarded markers must not leave orphan rows in stash.db.
    let live = if stash_db.exists() {
        tokenless_ccr::SqliteStore::new(&stash_db)
            .map(|s| s.len())
            .unwrap_or(0)
    } else {
        0
    };
    assert_eq!(
        live,
        0,
        "no-savings discard must roll back stash writes; orphan rows remain in {}",
        stash_db.display()
    );
}

#[test]
fn compress_schema_no_savings_rolls_back_orphan_stash() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };
    // Description just over the default 256-char cap: truncation + stash
    // marker often yields after_tokens >= before_tokens (1-char savings
    // does not always change the estimate). Pad the function name so the
    // compact JSON length hits that equality.
    let mut hit_no_savings = false;
    for name in ["x", "xx", "xxx", "xxxx"] {
        // Each candidate owns its database so a prior savings-path marker
        // cannot make this candidate's orphan-row assertion fail.
        let stash_db = fixture.data_dir.join(format!("stash-{name}.db"));
        let schema = serde_json::json!({
            "function": {
                "name": name,
                "description": "A".repeat(257),
                "parameters": {"type": "object", "properties": {}}
            }
        });
        let original = serde_json::to_string(&schema).unwrap();
        let output = fixture
            .command()
            .env("TOKENLESS_COMPRESSION_ENABLED", "1")
            .env("TOKENLESS_STATS_ENABLED", "0")
            .args(["compress-schema", "--stash-db"])
            .arg(&stash_db)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child.stdin.take().unwrap().write_all(original.as_bytes())?;
                child.wait_with_output()
            })
            .unwrap();
        assert!(
            output.status.success(),
            "compress-schema failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("did not reduce size") {
            continue;
        }
        hit_no_savings = true;
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains("tokenless:"),
            "no-savings stdout must not expose markers: {stdout}"
        );
        let live = if stash_db.exists() {
            tokenless_ccr::SqliteStore::new(&stash_db)
                .map(|s| s.len())
                .unwrap_or(0)
        } else {
            0
        };
        assert_eq!(
            live,
            0,
            "no-savings discard must roll back stash writes; orphan rows remain in {}",
            stash_db.display()
        );
        break;
    }
    assert!(
        hit_no_savings,
        "failed to hit compress-schema no-savings path with a just-over-limit description"
    );
}

#[test]
fn no_args_shows_error() {
    let output = tokenless_bin().output().unwrap();
    assert!(!output.status.success());
}

#[test]
fn invalid_json_input() {
    let output = tokenless_bin()
        .args(["compress-schema"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(b"not valid json")?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn compress_schema_with_agent_id() {
    let schema = r#"{"function":{"name":"test","parameters":{"type":"object","properties":{}}}}"#;
    let output = tokenless_bin()
        .args(["compress-schema", "--agent-id", "test-agent"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(schema.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(output.status.success());
}

#[test]
fn compress_response_with_session_and_tool_ids() {
    let response = r#"{"data":"value"}"#;
    let output = tokenless_bin()
        .args([
            "compress-response",
            "--agent-id",
            "test",
            "--session-id",
            "s1",
            "--tool-use-id",
            "t1",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(response.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(output.status.success());
}

#[test]
fn compress_toon_from_stdin() {
    let toon_input = r#"{"content":"some content","debug":"remove"}"#;
    let output = tokenless_bin()
        .args(["compress-toon"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(toon_input.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    // compress-toon may or may not succeed depending on input format
    let _ = output.status;
}

fn run_compress_toon(
    fixture: &TempDataDir,
    input: &str,
    compression_enabled: &str,
    session_id: &str,
    extra_args: &[&str],
) -> std::process::Output {
    fixture
        .command()
        .env("TOKENLESS_COMPRESSION_ENABLED", compression_enabled)
        .env("TOKENLESS_STATS_ENABLED", "1")
        .env("TOKENLESS_SLS_ENABLED", "0")
        .args([
            "compress-toon",
            "--agent-id",
            "integration-agent",
            "--session-id",
            session_id,
        ])
        .args(extra_args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(input.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap()
}

/// CJK payload where bytes/4 and the character estimator disagree. Dry-run
/// stderr must publish the same predicted counts that `stats summary` stores.
#[test]
fn compress_toon_dry_run_predicted_tokens_match_recorded_stats_for_cjk() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };
    // Repeated far enough past the 500-character TOON minimum that the
    // shared gate does not intercept this estimator-focused fixture.
    let input = serde_json::to_string(&serde_json::json!({
        "msg": "你好世界".repeat(130)
    }))
    .unwrap();
    assert!(input.chars().count() >= MIN_TOON_CHARS);
    assert_ne!(
        estimate_tokens(&input),
        estimate_tokens_from_bytes(input.len()),
        "fixture must be a CJK case where the two estimators disagree"
    );

    let output = run_compress_toon(&fixture, &input, "0", "cjk-toon-dry-run", &[]);
    assert!(
        output.status.success(),
        "compress-toon dry-run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim_end(),
        input,
        "dry-run must emit the original JSON"
    );

    let stderr = String::from_utf8(output.stderr).unwrap();
    let recorder = StatsRecorder::new(fixture.data_dir.join("stats.db")).unwrap();
    let records = recorder
        .records_by_session("cjk-toon-dry-run", None)
        .unwrap();
    assert_eq!(records.len(), 1);
    let predicted = format!(
        "predicted {} -> {} est. tokens",
        records[0].before_tokens, records[0].after_tokens
    );
    assert!(
        stderr.contains(&predicted),
        "dry-run stderr must match recorded stats, got stderr={stderr:?} stats={}:{} predicted={predicted}",
        records[0].before_tokens,
        records[0].after_tokens
    );
    assert_eq!(records[0].before_tokens, estimate_tokens(&input));
}

/// ASCII JSON where both estimators agree still encodes to TOON when smaller.
#[test]
fn compress_toon_ascii_emits_toon_when_character_estimator_saves() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };
    // Filler pushes the fixture past the 500-character TOON minimum while
    // staying pure ASCII, so the two estimators still agree.
    let filler_input = format!(
        r#"{{"content":"some content","debug":"remove","filler":"{}"}}"#,
        "a".repeat(520)
    );
    let input = filler_input.as_str();
    assert!(input.chars().count() >= MIN_TOON_CHARS);
    assert_eq!(
        estimate_tokens(input),
        estimate_tokens_from_bytes(input.len()),
        "ascii fixture should keep the two estimators in agreement"
    );

    let output = run_compress_toon(&fixture, input, "1", "ascii-toon-active", &[]);
    assert!(
        output.status.success(),
        "compress-toon failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let emitted = stdout.trim_end();
    assert_ne!(emitted, input, "active TOON with savings must replace JSON");
    assert!(
        emitted.contains("content:") || emitted.starts_with("content:"),
        "expected TOON object encoding, got {emitted:?}"
    );

    let recorder = StatsRecorder::new(fixture.data_dir.join("stats.db")).unwrap();
    let records = recorder
        .records_by_session("ascii-toon-active", None)
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].before_tokens, estimate_tokens(input));
    assert_eq!(records[0].after_tokens, estimate_tokens(emitted));
    assert!(records[0].after_tokens < records[0].before_tokens);
}

#[test]
fn compress_toon_cjk_active_emits_toon_and_records_character_tokens() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };
    let input = serde_json::to_string(&serde_json::json!({
        "msg": "你好世界".repeat(130)
    }))
    .unwrap();
    assert!(input.chars().count() >= MIN_TOON_CHARS);
    let output = run_compress_toon(&fixture, &input, "1", "cjk-toon-active", &[]);
    assert!(
        output.status.success(),
        "compress-toon failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let emitted = String::from_utf8(output.stdout).unwrap();
    let emitted = emitted.trim_end();
    assert_ne!(emitted, input);
    assert!(
        emitted.contains("msg:"),
        "expected TOON encoding, got {emitted:?}"
    );

    let recorder = StatsRecorder::new(fixture.data_dir.join("stats.db")).unwrap();
    let records = recorder
        .records_by_session("cjk-toon-active", None)
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].before_tokens, estimate_tokens(&input));
    assert_eq!(records[0].after_tokens, estimate_tokens(emitted));
}

/// Parse failures keep the documented compress-command exit code 2 now
/// that errors flow through the runtime mapping.
#[test]
fn compress_toon_invalid_json_exits_with_code_2() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };
    // Validation runs before the minimum-length gate, so a short invalid
    // payload fails with exit code 2 under the default threshold instead
    // of passing through untouched.
    let output = run_compress_toon(&fixture, "not json", "1", "toon-invalid-json", &[]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "invalid JSON under the default gate must exit 2, stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("JSON parse error"));

    // The same contract holds with the gate disabled.
    let output = run_compress_toon(
        &fixture,
        "not json",
        "1",
        "toon-invalid-json-gate-off",
        &["--min-toon-chars", "0"],
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "invalid JSON must exit 2 with the gate disabled, stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("JSON parse error"));
}

#[test]
fn env_check_without_spec() {
    let output = tokenless_bin().args(["env-check"]).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Specify --tool <name> or --all"));
}

#[test]
fn env_check_is_hard_bypassed_even_with_legacy_opt_in() {
    let output = tokenless_bin()
        .args(["env-check", "--tool", "Shell", "--json"])
        .env("TOKENLESS_TOOL_READY_ENABLED", "1")
        .env(
            "TOKENLESS_TOOL_READY_SPEC",
            "/path/that/must/not/be-read-while-disabled",
        )
        .output()
        .unwrap();

    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result.as_object().unwrap().len(), 3);
    assert_eq!(result["tool"], "Shell");
    assert_eq!(result["status"], "UNKNOWN");
    assert_eq!(result["enabled"], false);
}

#[test]
fn config_show() {
    let output = tokenless_bin().args(["config", "show"]).output().unwrap();
    // Should show current config or defaults
    let _ = output.status;
}

#[test]
fn stats_show_single_nonexistent() {
    let output = tokenless_bin()
        .args(["stats", "show", "99999"])
        .output()
        .unwrap();
    // Should fail gracefully for nonexistent record
    let _ = output.status;
}

#[test]
fn stats_diff_record_json_contains_structured_hunks() {
    let db = match TempStatsDb::new() {
        Some(db) => db,
        None => return,
    };
    let output = db
        .command()
        .args(["stats", "diff", &db.record_id.to_string(), "--json"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["schema_version"], "1.0");
    assert_eq!(json["scope"]["kind"], "record");
    assert_eq!(json["chains"][0]["diff"]["available"], true);
    assert!(json["chains"][0]["diff"]["hunks"].is_array());
}

#[test]
fn stats_diff_session_omits_content_hunks() {
    let db = match TempStatsDb::new() {
        Some(db) => db,
        None => return,
    };
    let output = db
        .command()
        .args([
            "stats",
            "diff",
            "--session",
            "integration-session",
            "--json",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["scope"]["kind"], "session");
    assert!(json["chains"][0].get("diff").is_none());
}

#[test]
fn stats_diff_tool_use_renders_terminal_diff() {
    let db = match TempStatsDb::new() {
        Some(db) => db,
        None => return,
    };
    let output = db
        .command()
        .args([
            "stats",
            "diff",
            "--session",
            "integration-session",
            "--tool-use-id",
            "integration-tool",
            "--no-color",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Estimated tokens: 10 -> 5"));
    assert!(stdout.contains("-remove"));
    assert!(!stdout.contains("\u{1b}["));
}

#[test]
fn stats_diff_invalid_scope_and_missing_record_use_expected_exit_codes() {
    let invalid = tokenless_bin()
        .args(["stats", "diff", "42", "--session", "session"])
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(2));

    let db = match TempStatsDb::new() {
        Some(db) => db,
        None => return,
    };
    let missing = db
        .command()
        .args(["stats", "diff", "999999"])
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(1));
}

fn write_checklist_spec(dir: &std::path::Path) -> std::path::PathBuf {
    // Config file that reliably exists for the config-file check.
    let config_path = dir.join("present.conf");
    std::fs::write(&config_path, "").unwrap();
    let spec = serde_json::json!({
        "_comment": "comment keys must be skipped",
        "Write": {
            "required": ["nonexistent_binary_xyz_98"],
            "recommended": [],
            "config_files": [],
            "permissions": [],
            "network": []
        },
        "Shell": {
            "required": ["bash"],
            "recommended": [],
            "config_files": [],
            "permissions": [],
            "network": []
        },
        "WebFetch": {
            "required": [],
            "recommended": ["nonexistent_binary_xyz_99"],
            "config_files": [],
            "permissions": [],
            "network": ["lan_probe"]
        },
        "Read": {
            "required": ["bash"],
            "recommended": [],
            "config_files": [config_path],
            "permissions": ["exec_shell"],
            "network": []
        }
    });
    let spec_path = dir.join("checklist-spec.json");
    std::fs::write(&spec_path, spec.to_string()).unwrap();
    spec_path
}

#[test]
fn env_check_checklist_json_is_hard_bypassed() {
    let dir = tempfile::tempdir().unwrap();
    let spec_path = write_checklist_spec(dir.path());

    let output = tokenless_bin()
        .args(["env-check", "--checklist", "--json"])
        .env("TOKENLESS_TOOL_READY_ENABLED", "1")
        .env("TOKENLESS_TOOL_READY_SPEC", &spec_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "env-check --checklist --json should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("--checklist --json must print one JSON object on stdout");
    assert_eq!(value.as_object().unwrap().len(), 3);
    assert_eq!(value["tool"], "checklist");
    assert_eq!(value["status"], "UNKNOWN");
    assert_eq!(value["enabled"], false);
    assert!(value.get("tools").is_none());
    assert!(value.get("summary").is_none());
}

#[test]
fn env_check_hard_bypass_json_is_stable_across_processes() {
    let dir = tempfile::tempdir().unwrap();
    let spec_path = write_checklist_spec(dir.path());

    let mut outputs = Vec::new();
    for _ in 0..8 {
        let output = tokenless_bin()
            .args(["env-check", "--checklist", "--json"])
            .env("TOKENLESS_TOOL_READY_ENABLED", "1")
            .env("TOKENLESS_TOOL_READY_SPEC", &spec_path)
            .output()
            .unwrap();
        assert!(output.status.success());
        outputs.push(output.stdout);
    }

    for (index, stdout) in outputs.iter().enumerate().skip(1) {
        assert_eq!(
            stdout,
            &outputs[0],
            "hard-bypass JSON must be byte-identical across processes (run {})",
            index + 1
        );
    }
}

#[test]
fn stats_summary_compare_rejects_missing_sessions() {
    let db = match TempStatsDb::new() {
        Some(db) => db,
        None => return,
    };
    let output = db
        .command()
        .args(["stats", "summary", "--compare", "missing-a", "missing-b"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("No records found"));
    assert!(stderr.contains("missing-a"));
    assert!(stderr.contains("missing-b"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("Tokenless Comparison Report"));
    assert!(!stdout.contains("saved_percent"));
}

#[test]
fn stats_summary_compare_json_rejects_one_missing_side() {
    let db = match TempStatsDb::new() {
        Some(db) => db,
        None => return,
    };
    let output = db
        .command()
        .args([
            "stats",
            "summary",
            "--json",
            "--compare",
            "missing-baseline",
            "integration-session",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("baseline session \"missing-baseline\""));
    assert!(!stderr.contains("tokenless session"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("saved_percent"));
}

#[test]
fn stats_summary_compare_reports_populated_sessions() {
    let db = match TempStatsDb::new() {
        Some(db) => db,
        None => return,
    };
    let recorder = StatsRecorder::new(&db.path).unwrap();
    recorder
        .record(
            &StatsRecord::new(
                OperationType::CompressResponse,
                "integration-agent".to_string(),
                1600,
                400,
                800,
                200,
            )
            .with_session_id("baseline-run")
            .with_mode(CompressionMode::DryRun),
        )
        .unwrap();
    recorder
        .record(
            &StatsRecord::new(
                OperationType::CompressResponse,
                "integration-agent".to_string(),
                1600,
                400,
                800,
                200,
            )
            .with_session_id("active-run")
            .with_mode(CompressionMode::Active),
        )
        .unwrap();

    let text = db
        .command()
        .args([
            "stats",
            "summary",
            "--compare",
            "baseline-run",
            "active-run",
        ])
        .output()
        .unwrap();
    assert!(
        text.status.success(),
        "compare populated sessions; stderr: {}",
        String::from_utf8_lossy(&text.stderr)
    );
    let stdout = String::from_utf8_lossy(&text.stdout);
    assert!(stdout.contains("Tokenless Comparison Report"));
    assert!(stdout.contains("TOTAL"));

    let json = db
        .command()
        .args([
            "stats",
            "summary",
            "--json",
            "--compare",
            "baseline-run",
            "active-run",
        ])
        .output()
        .unwrap();
    assert!(json.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(parsed["baseline_tokens"], 400);
    assert_eq!(parsed["tokenless_tokens"], 200);
    assert_eq!(parsed["saved_tokens"], 200);
}

#[test]
fn stats_summary_compare_rejects_zero_limit() {
    let db = match TempStatsDb::new() {
        Some(db) => db,
        None => return,
    };
    let recorder = StatsRecorder::new(&db.path).unwrap();
    recorder
        .record(
            &StatsRecord::new(
                OperationType::CompressResponse,
                "integration-agent".to_string(),
                1600,
                400,
                800,
                200,
            )
            .with_session_id("baseline-run")
            .with_mode(CompressionMode::DryRun),
        )
        .unwrap();
    recorder
        .record(
            &StatsRecord::new(
                OperationType::CompressResponse,
                "integration-agent".to_string(),
                1600,
                400,
                800,
                200,
            )
            .with_session_id("active-run")
            .with_mode(CompressionMode::Active),
        )
        .unwrap();

    let output = db
        .command()
        .args([
            "stats",
            "summary",
            "--limit",
            "0",
            "--compare",
            "baseline-run",
            "active-run",
        ])
        .output()
        .unwrap();
    assert_ne!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("greater than zero"),
        "zero limit must fail at parse time; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("No records found"),
        "populated sessions with --limit 0 must not look missing; stderr: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("Tokenless Comparison Report"));
    assert!(!stdout.contains("saved_percent"));
}

// ---- Protocol v2 `compress` lifecycle transport ----

fn spawn_with_stdin(
    command: &mut Command,
    args: &[&str],
    stdin_text: &str,
) -> std::process::Output {
    command
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write as _;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(stdin_text.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap()
}

#[test]
fn locked_stats_warn_without_changing_compression_or_retrieval() {
    let fixture = TempDataDir::new().expect("create isolated data directory");
    std::fs::create_dir_all(&fixture.data_dir).unwrap();
    let stats_db = fixture.data_dir.join("stats.db");
    let recorder = StatsRecorder::new(&stats_db).unwrap();
    let payload = "original payload\nwith no trailing newline";
    let stash = SqliteStore::new(fixture.data_dir.join("stash.db")).unwrap();
    let hash = stash.stash(payload).unwrap().key;
    let retrieve_request = serde_json::json!({
        "protocol_version": 2,
        "operation": "retrieve",
        "attribution": {"agent_id": "integration-agent"},
        "input": {"hash_or_marker": hash, "visible_markers": [hash]}
    })
    .to_string();
    let cases = [
        (
            vec!["compress-schema"],
            r#"{"function":{"name":"test","parameters":{"type":"object","properties":{"value":{"type":"string","title":"Remove Me","examples":["example"]}}}}}"#.to_string(),
        ),
        (
            vec!["compress"],
            post_tool_request_json(&debug_laden_content(), false, "locked-stats"),
        ),
        (vec!["retrieve", hash.as_str()], String::new()),
        (vec!["compress"], retrieve_request),
    ];

    for (args, input) in cases {
        let mut command = fixture.command();
        command
            .env("TOKENLESS_COMPRESSION_ENABLED", "1")
            .env("TOKENLESS_STATS_ENABLED", "1")
            .env("TOKENLESS_SLS_ENABLED", "0");
        let baseline = spawn_with_stdin(&mut command, &args, &input);
        assert!(baseline.status.success(), "{baseline:?}");
        assert!(baseline.stderr.is_empty(), "{baseline:?}");
        assert!(!baseline.stdout.is_empty());
        if args[0] == "retrieve" {
            assert_eq!(baseline.stdout, payload.as_bytes());
        }

        // Hold the writer lock before starting the child, and release it only
        // after the child exits; no sleep-based lock acquisition race.
        let conn = rusqlite::Connection::open(&stats_db).unwrap();
        conn.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let before: (i64, i64) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM stats), (SELECT COUNT(*) FROM retrieve_events)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        for enabled in ["1", "0"] {
            let output = spawn_with_stdin(
                fixture
                    .command()
                    .env("TOKENLESS_COMPRESSION_ENABLED", "1")
                    .env("TOKENLESS_STATS_ENABLED", enabled)
                    .env("TOKENLESS_SLS_ENABLED", "0"),
                &args,
                &input,
            );
            assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
            assert_eq!(output.stdout, baseline.stdout, "{args:?}");
            let stderr = String::from_utf8(output.stderr).unwrap();
            if enabled == "1" {
                assert_eq!(
                    stderr.matches("[tokenless-stats] WARNING:").count(),
                    1,
                    "{args:?}: {stderr}"
                );
                assert!(stderr.contains("database is locked"), "{args:?}: {stderr}");
            } else {
                assert!(stderr.is_empty(), "{args:?}: {stderr}");
            }
        }
        conn.execute_batch("ROLLBACK").unwrap();
        let after: (i64, i64) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM stats), (SELECT COUNT(*) FROM retrieve_events)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(after, before, "failed or disabled stats must not add rows");
    }
    assert!(recorder.retrieve_totals().unwrap().hits >= 2);
}

fn post_tool_request_json(content: &str, recovery_available: bool, session_id: &str) -> String {
    serde_json::json!({
        "protocol_version": 2,
        "operation": "post_tool",
        "attribution": {
            "agent_id": "integration-agent",
            "session_id": session_id,
            "tool_use_id": "call-1"
        },
        "input": {
            "result_kind": "tool",
            "tool_name": "WebFetch",
            "content": content,
            "status": "success",
            "content_origin": "api_response",
            "output_optimization": "none",
            "capabilities": {
                "replace_output": true,
                "recovery": {"kind": if recovery_available { "shell" } else { "none" }},
                "replace_with_text": false
            }
        }
    })
    .to_string()
}

fn debug_laden_content() -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "url": "https://registry.example.com/packages",
        "status": 200,
        "debug": "cache=miss upstream=registry-04 trace=9f2e11c0 backend_latency_ms=184 retries=0",
        "results": (0..8).map(|i| serde_json::json!({
            "name": format!("pkg-{i}"),
            "version": "1.0.0",
            "license": null,
            "homepage": "",
        })).collect::<Vec<_>>(),
        "count": 8,
    }))
    .unwrap()
}

#[test]
fn compress_post_tool_uses_the_v2_result_envelope() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };
    let content = debug_laden_content();
    let output = spawn_with_stdin(
        fixture
            .command()
            .env("TOKENLESS_COMPRESSION_ENABLED", "1")
            .env("TOKENLESS_STATS_ENABLED", "0")
            .env("TOKENLESS_SLS_ENABLED", "0"),
        &["compress"],
        &post_tool_request_json(&content, false, "v2-post-tool"),
    );
    assert!(
        output.status.success(),
        "compress failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["protocol_version"], 2);
    assert_eq!(response["operation"], "post_tool");
    assert_eq!(response["result"]["disposition"], "applied");
    assert_eq!(
        response["result"]["applied_operations"],
        serde_json::json!(["json_cleanup"])
    );
    assert_eq!(response["result"]["recoverability"], "lossless");
    assert_eq!(response["result"]["content_type"], "json");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(response["result"]["output"].as_str().unwrap())
            .unwrap(),
        serde_json::from_str::<serde_json::Value>(&content).unwrap()
    );
}

#[test]
fn compress_post_tool_dry_run_previews_record_reduction_without_stash_writes() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };
    let records = (0..200)
        .map(|index| {
            serde_json::json!({
                "id": index,
                "message": format!("record-{index}-{}", "x".repeat(80))
            })
        })
        .collect::<Vec<_>>();
    let content = serde_json::to_string(&records).unwrap();
    let output = spawn_with_stdin(
        fixture
            .command()
            .env("TOKENLESS_COMPRESSION_ENABLED", "0")
            .env("TOKENLESS_STATS_ENABLED", "0")
            .env("TOKENLESS_SLS_ENABLED", "0"),
        &["compress"],
        &post_tool_request_json(&content, true, "record-dry-run"),
    );

    assert!(
        output.status.success(),
        "compress failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["result"]["disposition"], "dry_run");
    assert_eq!(response["result"]["output"], content);
    assert!(
        response["result"]["after_tokens"].as_u64().unwrap()
            < response["result"]["before_tokens"].as_u64().unwrap()
    );

    let store = SqliteStore::new(fixture.data_dir.join("stash.db")).unwrap();
    assert_eq!(store.len(), 0);
}

fn build_log_request(content: &str, status: &str, session_id: &str) -> String {
    serde_json::json!({
        "protocol_version": 2,
        "operation": "post_tool",
        "attribution": {
            "agent_id": "integration-agent",
            "session_id": session_id,
            "tool_use_id": "call-1"
        },
        "input": {
            "result_kind": "tool",
            "tool_name": "Bash",
            "content": content,
            "status": status,
            "content_origin": "command_output",
            "output_optimization": "none",
            "capabilities": {
                "replace_output": true,
                "recovery": {"kind": "shell"},
                "replace_with_text": true
            }
        }
    })
    .to_string()
}

#[test]
fn diff_opt_in_round_trips_git_patch_and_original() {
    for (ending, quote_path) in [
        ("\n", "true"),
        ("\n", "false"),
        ("\r\n", "true"),
        ("\r\n", "false"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .current_dir(temp.path())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap()
        };
        git(&["init", "-q"]);
        git(&["config", "core.autocrlf", "false"]);
        git(&["config", "core.quotePath", quote_path]);
        let mut paths: Vec<std::path::PathBuf> = [
            "f",
            "user settings.py",
            "trailing ",
            "dir b/name",
            "中文.py",
            "quote\".py",
            "slash\\.py",
            "tab\t.py",
            "newline\n.py",
            "controls\x07\x08\x0b\x0c\r.py",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        #[cfg(windows)]
        paths.retain(|path| {
            !path.to_string_lossy().ends_with(' ')
                && !path
                    .to_string_lossy()
                    .chars()
                    .any(|ch| ch.is_control() || ch == '"' || ch == '\\')
        });
        // With quotePath=false these bytes are emitted raw, outside the
        // text input accepted by the compression API.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            if quote_path == "true" {
                paths.push(std::ffi::OsString::from_vec(b"non-utf8-\xff.py".to_vec()).into());
            }
        }
        let original: String = (0..24)
            .map(|i| format!("line {i:02}: {}{ending}", "unchanged context ".repeat(4)))
            .collect();
        let modified = original.replace("line 12:", "changed:");
        for path in &paths {
            let path = temp.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, &original).unwrap();
        }
        git(&["add", "."]);
        let base = git(&["write-tree"]);
        for path in &paths {
            std::fs::write(temp.path().join(path), &modified).unwrap();
        }
        let diff = git(&[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "-U10",
        ]);
        git(&["add", "."]);
        let target = git(&["write-tree"]);
        git(&["read-tree", base.trim()]);
        for path in &paths {
            std::fs::write(temp.path().join(path), &original).unwrap();
        }
        git(&["update-index", "--refresh"]);

        let state = temp.path().join("data");
        for enabled in [None, Some("0"), Some("1"), Some("TRUE"), Some("yes")] {
            let mut command = tokenless_bin();
            command
                .env("TOKENLESS_DATA_DIR", &state)
                .env_remove("TOKENLESS_STATS_DB")
                .env_remove("TOKENLESS_STASH_DB")
                .env_remove("TOKENLESS_DIFF_COMPRESSION_ENABLED")
                .env("TOKENLESS_COMPRESSION_ENABLED", "1")
                .env("TOKENLESS_STATS_ENABLED", "0")
                .env("TOKENLESS_SLS_ENABLED", "0");
            if let Some(value) = enabled {
                command.env("TOKENLESS_DIFF_COMPRESSION_ENABLED", value);
            }
            let output = spawn_with_stdin(
                &mut command,
                &["compress"],
                &build_log_request(&diff, "success", "diff-integration"),
            );
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            let result = &response["result"];
            if enabled.is_none() || enabled == Some("0") {
                assert_eq!(result["disposition"], "passthrough");
                assert_eq!(result["output"], diff);
                assert_eq!(result["stash_keys"], serde_json::json!([]));
                continue;
            }
            assert_eq!(
                result["applied_operations"],
                serde_json::json!(["diff_reduction"])
            );
            assert_eq!(result["recoverability"], "retrievable");
            let compressed = result["output"].as_str().unwrap();
            assert!(compressed.len() < diff.len());
            let hash = result["stash_keys"][0].as_str().unwrap();
            let retrieved = tokenless_bin()
                .env("TOKENLESS_DATA_DIR", &state)
                .env_remove("TOKENLESS_STASH_DB")
                .env("TOKENLESS_STATS_ENABLED", "0")
                .env("TOKENLESS_SLS_ENABLED", "0")
                .args(["retrieve", hash])
                .output()
                .unwrap();
            assert!(retrieved.status.success());
            assert_eq!(retrieved.stdout, diff.as_bytes());

            let patch = &compressed[compressed.find("diff --git ").unwrap()..];
            let patch = patch.strip_suffix("\n[End diff]").unwrap();
            let original_sections: Vec<_> = diff.split("diff --git ").skip(1).collect();
            let compressed_sections: Vec<_> = patch.split("diff --git ").skip(1).collect();
            assert_eq!(original_sections.len(), paths.len());
            assert_eq!(compressed_sections.len(), paths.len());
            for (original, compressed) in original_sections.iter().zip(&compressed_sections) {
                assert!(compressed.len() < original.len(), "{compressed}");
                assert_eq!(original.split("@@ ").next(), compressed.split("@@ ").next());
                let changes = |section: &str| {
                    section
                        .split_once("@@ ")
                        .unwrap()
                        .1
                        .split_inclusive('\n')
                        .skip(1)
                        .filter(|line| line.starts_with(['+', '-']))
                        .collect::<String>()
                };
                assert_eq!(changes(original), changes(compressed));
            }
            let patch_path = temp.path().join("candidate.patch");
            std::fs::write(&patch_path, patch).unwrap();
            git(&[
                "apply",
                "--index",
                "--whitespace=nowarn",
                patch_path.to_str().unwrap(),
            ]);
            assert_eq!(git(&["write-tree"]), target);
            for path in &paths {
                assert_eq!(
                    std::fs::read(temp.path().join(path)).unwrap(),
                    modified.as_bytes()
                );
            }
            git(&["read-tree", base.trim()]);
            for path in &paths {
                std::fs::write(temp.path().join(path), &original).unwrap();
            }
            git(&["update-index", "--refresh"]);
        }
    }
}

#[test]
fn compress_post_tool_reduces_and_restores_build_logs() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };
    let mut content = "$ cargo build\n".to_owned();
    for index in 0..30 {
        content.push_str(&format!(
            "Compiling package-{index:03} v0.1.{index} with extended progress output\n"
        ));
    }
    content.push_str("Finished `dev` profile [unoptimized] target(s) in 1.2s\n");
    let output = spawn_with_stdin(
        fixture
            .command()
            .env("TOKENLESS_COMPRESSION_ENABLED", "1")
            .env("TOKENLESS_STATS_ENABLED", "0")
            .env("TOKENLESS_SLS_ENABLED", "0"),
        &["compress"],
        &build_log_request(&content, "success", "build-log"),
    );
    assert!(
        output.status.success(),
        "compress failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["result"]["disposition"], "applied");
    assert_eq!(response["result"]["content_type"], "build_log");
    assert_eq!(
        response["result"]["applied_operations"],
        serde_json::json!(["build_log_reduction"])
    );
    assert_eq!(response["result"]["recoverability"], "retrievable");
    let compressed = response["result"]["output"].as_str().unwrap();
    let hash = extract_hash(compressed).unwrap();
    let retrieve = fixture
        .command()
        .env("TOKENLESS_STATS_ENABLED", "0")
        .args(["retrieve", hash])
        .output()
        .unwrap();
    assert!(retrieve.status.success());
    assert!(
        String::from_utf8(retrieve.stdout)
            .unwrap()
            .contains("package-015")
    );
}

#[test]
fn compress_post_tool_unittest_logs_preserve_summary_and_recover_original() {
    let fixture = TempDataDir::new().unwrap();
    let mut content = (0..83)
        .map(|index| format!("test_case_{index:03} (__main__.ExampleTests) ... ok\n"))
        .collect::<String>();
    content.push_str(
        "\n----------------------------------------------------------------------\nRan 83 tests in 0.295s\n\nOK\n",
    );
    for (status, recovery, disposition) in [
        ("success", "shell", "applied"),
        ("success", "none", "no_savings"),
        ("error", "shell", "tool_error"),
    ] {
        let mut request: serde_json::Value =
            serde_json::from_str(&build_log_request(&content, status, "unittest-log")).unwrap();
        request["input"]["capabilities"]["recovery"]["kind"] = recovery.into();
        request["input"]["command"] = "python -m unittest -v".into();
        let output = spawn_with_stdin(
            fixture
                .command()
                .env("TOKENLESS_COMPRESSION_ENABLED", "1")
                .env("TOKENLESS_STATS_ENABLED", "0")
                .env("TOKENLESS_SLS_ENABLED", "0"),
            &["compress"],
            &request.to_string(),
        );
        assert!(output.status.success(), "{:?}", output);
        let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let result = &response["result"];
        assert_eq!(result["content_type"], "build_log");
        assert_eq!(result["disposition"], disposition);
        if disposition != "applied" {
            assert_eq!(result["output"], content);
            assert_eq!(result["applied_operations"], serde_json::json!([]));
            continue;
        }
        assert_eq!(result["recoverability"], "retrievable");
        assert_eq!(
            result["applied_operations"],
            serde_json::json!(["build_log_reduction"])
        );
        assert!(
            result["after_tokens"].as_u64().unwrap() < result["before_tokens"].as_u64().unwrap()
        );
        let compressed = result["output"].as_str().unwrap();
        assert!(compressed.ends_with("Ran 83 tests in 0.295s\n\nOK\n"));
        let hash = extract_hash(compressed).unwrap();
        let retrieve = fixture.command().args(["retrieve", hash]).output().unwrap();
        assert!(retrieve.status.success());
        let marker = compressed.lines().find(|line| line.contains(hash)).unwrap();
        assert_eq!(
            compressed.replace(
                &format!("{marker}\n"),
                &String::from_utf8(retrieve.stdout).unwrap(),
            ),
            content
        );
    }
}

#[test]
fn compress_post_tool_error_build_log_stays_original_with_diagnosis() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };
    let mut content = "$ cargo build\n".to_owned();
    for index in 0..30 {
        content.push_str(&format!(
            "Compiling package-{index:03} v0.1.{index} with extended progress output\n"
        ));
    }
    content.push_str("error: linker command not found\n");
    let output = spawn_with_stdin(
        fixture
            .command()
            .env("TOKENLESS_COMPRESSION_ENABLED", "1")
            .env("TOKENLESS_STATS_ENABLED", "0")
            .env("TOKENLESS_SLS_ENABLED", "0"),
        &["compress"],
        &build_log_request(&content, "error", "build-log-error"),
    );
    assert!(
        output.status.success(),
        "compress failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["result"]["disposition"], "tool_error");
    assert_eq!(response["result"]["content_type"], "build_log");
    assert_eq!(response["result"]["output"], content);
    assert_eq!(
        response["result"]["applied_operations"],
        serde_json::json!([])
    );
    assert!(
        response["result"]["additional_context"]
            .as_str()
            .unwrap()
            .contains("ENV_DEPENDENCY_MISSING")
    );
}

#[test]
fn compress_before_model_and_retrieve_operations_are_dispatched() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };
    let request = serde_json::json!({
        "protocol_version": 2,
        "operation": "before_model",
        "attribution": {"agent_id": "integration-agent"},
        "input": {
            "tools": [{
                "type": "function",
                "function": {
                    "name": "read",
                    "description": "verbose description ".repeat(200),
                    "parameters": {"type": "object", "properties": {}}
                }
            }],
            "visible_context": {"messages": []},
            "capabilities": {
                "replace_tools": true,
                "recovery": {"kind": "tool", "name": "tokenless_retrieve"}
            }
        }
    });
    let output = spawn_with_stdin(
        fixture
            .command()
            .env("TOKENLESS_COMPRESSION_ENABLED", "1")
            .env("TOKENLESS_STATS_ENABLED", "0"),
        &["compress"],
        &request.to_string(),
    );
    assert!(output.status.success());
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let hash = response["result"]["visible_markers"][0]
        .as_str()
        .unwrap()
        .to_string();
    assert!(response["result"].get("retrieve_tool").is_none());

    let retrieve = serde_json::json!({
        "protocol_version": 2,
        "operation": "retrieve",
        "attribution": {"agent_id": "integration-agent"},
        "input": {
            "hash_or_marker": hash,
            "visible_markers": [hash]
        }
    });
    let output = spawn_with_stdin(
        fixture.command().env("TOKENLESS_STATS_ENABLED", "0"),
        &["compress"],
        &retrieve.to_string(),
    );
    assert!(output.status.success());
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["operation"], "retrieve");
    assert!(
        response["result"]["payload"]
            .as_str()
            .unwrap()
            .contains("verbose description")
    );
}

#[test]
#[cfg(unix)]
fn compress_pre_tool_resolves_the_independent_rtk_binary() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };
    let bin_dir = tempfile::tempdir().unwrap();
    let rtk = bin_dir.path().join("rtk");
    std::fs::write(
        &rtk,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf 'rtk 0.43.0'; exit 0; fi\nprintf 'rtk grep --count error log'\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(&rtk).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&rtk, permissions).unwrap();
    }
    let request = serde_json::json!({
        "protocol_version": 2,
        "operation": "pre_tool",
        "attribution": {"agent_id": "integration-agent"},
        "input": {
            "tool_name": "Bash",
            "arguments": {"command": "grep error log"},
            "command_field": "command",
            "capabilities": {
                "replace_arguments": true,
                "block_and_suggest": false
            }
        }
    });
    let output = spawn_with_stdin(
        fixture
            .command()
            .env(
                "PATH",
                std::env::join_paths(std::iter::once(bin_dir.path().to_path_buf()).chain(
                    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
                ))
                .unwrap(),
            )
            .env("TOKENLESS_STATS_ENABLED", "0"),
        &["compress"],
        &request.to_string(),
    );
    assert!(output.status.success());
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["result"]["action"], "replace_arguments");
    assert_eq!(response["result"]["output_optimization"], "rtk");
}

#[test]
fn compress_exit_contract_rejects_v1_and_operation_failures() {
    let v1 = r#"{"protocol_version":1,"content":"x","agent_id":"a","seam":"post_tool"}"#;
    let output = spawn_with_stdin(&mut tokenless_bin(), &["compress"], v1);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());

    let unauthorized = serde_json::json!({
        "protocol_version": 2,
        "operation": "retrieve",
        "attribution": {"agent_id": "integration-agent"},
        "input": {
            "hash_or_marker": "0123456789abcdef01234567",
            "visible_markers": []
        }
    });
    let output = spawn_with_stdin(
        &mut tokenless_bin(),
        &["compress"],
        &unauthorized.to_string(),
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());

    let post_tool = post_tool_request_json(
        &serde_json::json!({"records": [{"value": "x".repeat(2000)}]}).to_string(),
        true,
        "database-error",
    );
    let output = spawn_with_stdin(
        tokenless_bin()
            .env("TOKENLESS_DATA_DIR", "relative/data")
            .env("TOKENLESS_STATS_ENABLED", "0"),
        &["compress"],
        &post_tool,
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
}

#[test]
fn mcp_command_is_absent() {
    let output = tokenless_bin().args(["mcp", "serve"]).output().unwrap();
    assert_ne!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unrecognized subcommand"));
}

/// Regression for GH-2838: a short payload with real token savings must
/// pass through unchanged by default, matching the adapter hooks' shared
/// 500-character minimum instead of being TOON-encoded.
#[test]
fn compress_toon_short_payload_passes_through_by_default() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };
    let input = r#"{"a":"short"}"#;
    assert!(input.chars().count() < MIN_TOON_CHARS);

    let output = run_compress_toon(&fixture, input, "1", "toon-min-skip", &[]);
    assert!(
        output.status.success(),
        "compress-toon failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Byte-for-byte contract: stdout must equal the input payload with no
    // bytes added or stripped (in particular no appended trailing LF), so
    // automation can detect passthrough by comparing stdout with the input.
    let emitted = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        emitted, input,
        "payloads under the minimum must pass through byte-for-byte"
    );
    // Legitimate trailing whitespace must survive the gate too: a trailing
    // LF in the input must neither be stripped nor doubled.
    let input_lf = format!("{input}\n");
    let output = run_compress_toon(&fixture, &input_lf, "1", "toon-min-skip-lf", &[]);
    assert!(
        output.status.success(),
        "compress-toon failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let emitted = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        emitted, input_lf,
        "passthrough must preserve a trailing newline byte-for-byte"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("TOON minimum") && stderr.contains("skipping encoding"),
        "expected the minimum-length skip note on stderr, got {stderr:?}"
    );

    // Skipped payloads record no stats, same as no-savings runs; the
    // recorder database is never created when nothing was recorded.
    let stats_db = fixture.data_dir.join("stats.db");
    if stats_db.exists() {
        let recorder = StatsRecorder::new(stats_db).unwrap();
        let records = recorder.records_by_session("toon-min-skip", None).unwrap();
        assert!(records.is_empty());
    }
}

/// `--min-toon-chars 0` restores the pre-gate behavior for callers that
/// explicitly want short payloads encoded.
#[test]
fn compress_toon_min_chars_zero_encodes_short_payload() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };
    let input = r#"{"a":"short"}"#;

    let output = run_compress_toon(
        &fixture,
        input,
        "1",
        "toon-min-force",
        &["--min-toon-chars", "0"],
    );
    assert!(
        output.status.success(),
        "compress-toon --min-toon-chars 0 failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Encoded output is emitted byte-for-byte as well: the TOON text is
    // already trimmed by the runtime, so no trailing LF is appended.
    let emitted = String::from_utf8(output.stdout).unwrap();
    assert_ne!(emitted, input, "min-chars 0 must encode a saving payload");
    assert_eq!(emitted, "a: short", "expected TOON object encoding");
}

/// 40 uniform records keyed by ID. TOON spec v4.4 encodes this shape as a
/// keyed tabular map, which is shorter than compact JSON; 0.5.0 expanded every
/// entry into an indented block and never reached the encoder.
fn keyed_record_map(ratio: &str) -> String {
    let entries = (0..40)
        .map(|index| {
            let ratio = if index == 3 { ratio } else { "0.5" };
            format!(r#""u{index}":{{"name":"user{index}","active":true,"ratio":{ratio}}}"#)
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(r#"{{"users":{{{entries}}}}}"#)
}

/// TOON's numeric domain is narrower than JSON's. Spec v4.4 brought keyed
/// object maps inside the size gate, so the direct `compress-toon` entry has to
/// apply the same fidelity gate the PostTool JSON domain applies instead of
/// silently writing `1e-400` as the integer `0`.
#[test]
fn compress_toon_keeps_original_json_when_numbers_would_not_survive() {
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };

    // Control: the same shape without an offending literal is still encoded,
    // so the passthroughs below are the numeric gate and not a size accident.
    let clean = keyed_record_map("0.5");
    let output = run_compress_toon(&fixture, &clean, "1", "toon-numeric-clean", &[]);
    assert!(
        output.status.success(),
        "compress-toon failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let encoded = String::from_utf8(output.stdout).unwrap();
    assert!(
        encoded.starts_with("users[40:]{name,active,ratio}:"),
        "expected a keyed tabular map, got {encoded:?}"
    );

    // `1e-400` is caught by the literal scan; `-0.0` and `1e3` survive the
    // scan and are caught by the decode round trip, which sees the lost sign
    // and the float-to-integer type change.
    for ratio in ["1e-400", "-0.0", "1e3"] {
        let input = keyed_record_map(ratio);
        let output = run_compress_toon(&fixture, &input, "1", "toon-numeric-lossy", &[]);
        assert!(
            output.status.success(),
            "compress-toon failed for ratio {ratio}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let emitted = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            emitted, input,
            "ratio {ratio} must pass through byte-for-byte"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("cannot retain") && stderr.contains("skipping encoding"),
            "expected the numeric skip note for ratio {ratio}, got {stderr:?}"
        );
    }

    // A declined payload books no savings, same as the length gate.
    let stats_db = fixture.data_dir.join("stats.db");
    if stats_db.exists() {
        let recorder = StatsRecorder::new(stats_db).unwrap();
        let records = recorder
            .records_by_session("toon-numeric-lossy", None)
            .unwrap();
        assert!(records.is_empty());
    }
}

/// Every advisory note on the `compress-toon` path is emitted after the payload
/// and through a write whose failure is discarded. `eprintln!` panics when the
/// write fails, so stderr redirected to a full filesystem used to turn a
/// documented "exit 0, original JSON on stdout" passthrough into exit code 101
/// with an empty stdout.
#[test]
fn compress_toon_passthrough_survives_an_unwritable_stderr() {
    // `/dev/full` accepts the write and then reports ENOSPC, which is what a
    // full log filesystem looks like to the process. Nothing portable
    // reproduces it, so hosts without the device skip the case.
    let dev_full = std::path::Path::new("/dev/full");
    if !dev_full.exists() {
        return;
    }
    let fixture = match TempDataDir::new() {
        Some(fixture) => fixture,
        None => return,
    };

    // One case per advisory note. Each payload is a documented passthrough, so
    // stdout has to reproduce it byte for byte whatever stderr does.
    struct Case {
        label: &'static str,
        input: String,
        args: &'static [&'static str],
        compression_enabled: &'static str,
    }
    let cases = [
        Case {
            label: "numeric gate",
            input: keyed_record_map("1e3"),
            args: &[],
            compression_enabled: "1",
        },
        Case {
            label: "length gate",
            input: r#"{"a":"short"}"#.to_owned(),
            args: &[],
            compression_enabled: "1",
        },
        Case {
            label: "no savings",
            input: "{}".to_owned(),
            args: &["--min-toon-chars", "0"],
            compression_enabled: "1",
        },
        Case {
            label: "dry run",
            input: keyed_record_map("0.5"),
            args: &[],
            compression_enabled: "0",
        },
    ];

    for case in cases {
        let stderr = std::fs::OpenOptions::new()
            .write(true)
            .open(dev_full)
            .unwrap();
        let output = fixture
            .command()
            .env("TOKENLESS_COMPRESSION_ENABLED", case.compression_enabled)
            .env("TOKENLESS_STATS_ENABLED", "1")
            .env("TOKENLESS_SLS_ENABLED", "0")
            .args([
                "compress-toon",
                "--agent-id",
                "integration-agent",
                "--session-id",
                "toon-unwritable-stderr",
            ])
            .args(case.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::from(stderr))
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(case.input.as_bytes())?;
                child.wait_with_output()
            })
            .unwrap();
        assert!(
            output.status.success(),
            "{}: an unwritable stderr must not fail compress-toon, got {:?}",
            case.label,
            output.status
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            case.input,
            "{}: the payload must still reach stdout byte for byte",
            case.label
        );
    }

    // Control: with a writable stderr the notes are still there, so the cases
    // above prove the write is tolerated and not that it was dropped.
    let numeric = keyed_record_map("1e3");
    let output = run_compress_toon(&fixture, &numeric, "1", "toon-writable-stderr", &[]);
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("cannot retain"),
        "the advisory note must still be printed when stderr is writable"
    );
}

#[test]
fn html_extraction_is_env_gated_and_recovers_the_original_page() {
    let temp = tempfile::tempdir().unwrap();
    let html = format!(
        "<!DOCTYPE html>\n<html><head><title>Guide</title>\
         <link rel=\"canonical\" href=\"https://example.com/guide\"><style>{}</style></head>\
         <body><nav><a href=\"/\">Home</a></nav><main><h1>Guide</h1>{}\
         <table><tr><th>Flag</th><th>Meaning</th></tr><tr><td>-v</td><td>verbose</td></tr></table>\
         <pre><code class=\"language-sh\">tokenless retrieve HASH</code></pre></main>\
         <footer>footer</footer><script>{}</script></body></html>",
        "p{margin:0}".repeat(40),
        (0..8)
            .map(|i| format!("<p>Section {i}: {}</p>", "explanatory prose ".repeat(10)))
            .collect::<String>(),
        "window.x = 1;".repeat(40)
    );
    let state = temp.path().join("data");
    // An empty value counts as unset and keeps the default; `on` is not a
    // recognised true value and disables rendering.
    for enabled in [
        None,
        Some(""),
        Some("0"),
        Some("false"),
        Some("on"),
        Some("1"),
        Some("TRUE"),
        Some("yes"),
    ] {
        let mut command = tokenless_bin();
        command
            .env("TOKENLESS_DATA_DIR", &state)
            .env_remove("TOKENLESS_STATS_DB")
            .env_remove("TOKENLESS_STASH_DB")
            .env_remove("TOKENLESS_HTML_EXTRACTION_ENABLED")
            .env("TOKENLESS_COMPRESSION_ENABLED", "1")
            .env("TOKENLESS_STATS_ENABLED", "0")
            .env("TOKENLESS_SLS_ENABLED", "0");
        if let Some(value) = enabled {
            command.env("TOKENLESS_HTML_EXTRACTION_ENABLED", value);
        }
        let output = spawn_with_stdin(
            &mut command,
            &["compress"],
            &build_log_request(&html, "success", "html-integration"),
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let result = &response["result"];
        assert_eq!(result["content_type"], "html");
        if matches!(enabled, Some("0") | Some("false") | Some("on")) {
            assert_eq!(result["disposition"], "passthrough");
            assert_eq!(result["output"], html);
            assert_eq!(result["stash_keys"], serde_json::json!([]));
            continue;
        }
        assert_eq!(result["disposition"], "applied");
        assert_eq!(
            result["applied_operations"],
            serde_json::json!(["html_extraction"])
        );
        assert_eq!(result["recoverability"], "retrievable");
        let rendered = result["output"].as_str().unwrap();
        assert!(rendered.len() < html.len());
        assert!(rendered.contains(
            "[HTML page rendered as Markdown; <main> only, 3 nodes outside it omitted; \
             removed 1 style. Retrieve original for the full page.]\nTitle: Guide\nURL: https://example.com/guide\n\
             # Guide\n\nSection 0: explanatory prose"
        ));
        assert!(rendered.contains("| Flag | Meaning |\n| --- | --- |\n| -v | verbose |"));
        assert!(rendered.contains("```sh\ntokenless retrieve HASH\n```"));
        assert!(!rendered.contains("Home") && !rendered.contains("window.x"));
        let hash = result["stash_keys"][0].as_str().unwrap();
        assert!(rendered.starts_with(&format!(
            "If needed, run in shell: tokenless retrieve {hash}\n"
        )));
        let retrieved = tokenless_bin()
            .env("TOKENLESS_DATA_DIR", &state)
            .env_remove("TOKENLESS_STASH_DB")
            .env("TOKENLESS_STATS_ENABLED", "0")
            .env("TOKENLESS_SLS_ENABLED", "0")
            .args(["retrieve", hash])
            .output()
            .unwrap();
        assert!(retrieved.status.success());
        assert_eq!(retrieved.stdout, html.as_bytes());
    }
}

#[test]
fn html_zero_rowspan_preserves_columns_and_recovers_the_page() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("data");
    let html = format!(
        "<!DOCTYPE html><html><head><title>Service inventory</title></head><body>\
         <p>This service inventory records deployment regions, service names, \
         and instance counts for the production environment.</p>\
         <table><thead><tr><th>Region</th><th>Service</th><th>Count</th></tr></thead>\
         <tbody><tr><td rowspan=\"0\">EU</td><td>api</td><td>10</td></tr>\
         <tr><td>worker</td><td>20</td></tr></tbody></table>\
         <script>{}</script></body></html>",
        "var padding = 12345; ".repeat(100)
    );
    let mut request: serde_json::Value =
        serde_json::from_str(&post_tool_request_json(&html, true, "html-zero-rowspan")).unwrap();
    request["input"]["capabilities"]["replace_with_text"] = true.into();
    let output = spawn_with_stdin(
        tokenless_bin()
            .env("TOKENLESS_DATA_DIR", &state)
            .env_remove("TOKENLESS_STATS_DB")
            .env_remove("TOKENLESS_STASH_DB")
            .env("TOKENLESS_HTML_EXTRACTION_ENABLED", "1")
            .env("TOKENLESS_COMPRESSION_ENABLED", "1")
            .env("TOKENLESS_STATS_ENABLED", "0")
            .env("TOKENLESS_SLS_ENABLED", "0"),
        &["compress"],
        &request.to_string(),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let result = &response["result"];
    assert_eq!(result["disposition"], "applied");
    assert_eq!(
        result["applied_operations"],
        serde_json::json!(["html_extraction"])
    );
    assert_eq!(result["recoverability"], "retrievable");
    let rendered = result["output"].as_str().unwrap();
    assert!(
        rendered.contains(
            "| Region | Service | Count |\n| --- | --- | --- |\n\
         | EU | api | 10 |\n|  | worker | 20 |"
        ),
        "{rendered}"
    );
    let hash = result["stash_keys"][0].as_str().unwrap();
    let retrieved = tokenless_bin()
        .env("TOKENLESS_DATA_DIR", &state)
        .env_remove("TOKENLESS_STASH_DB")
        .env("TOKENLESS_STATS_ENABLED", "0")
        .env("TOKENLESS_SLS_ENABLED", "0")
        .args(["retrieve", hash])
        .output()
        .unwrap();
    assert!(retrieved.status.success());
    assert_eq!(retrieved.stdout, html.as_bytes());
}

// ─────────────────────────────────────────────────────────────────────
// Fail-soft warnings must survive an unwritable stderr (fd 2 → /dev/full)
// ─────────────────────────────────────────────────────────────────────
// `eprintln!` panics when writing to stderr fails (a full filesystem
// behind redirected logs, a closed descriptor). Every warning emitted on
// a fail-soft path inside the stats, SLS and stash libraries must
// instead discard its own write error: the stats layer stays invisible
// to the compression and retrieval results.

/// Open /dev/full for a child's stderr: every write fails with ENOSPC,
/// the production shape of "stderr cannot be written".
#[cfg(target_os = "linux")]
fn dev_full() -> std::fs::File {
    std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("open /dev/full")
}

#[test]
#[cfg(target_os = "linux")]
fn stats_open_warning_survives_an_unwritable_stderr() {
    let fixture = TempDataDir::new().expect("create isolated data directory");
    std::fs::create_dir_all(&fixture.data_dir).unwrap();
    std::fs::write(fixture.data_dir.join("stats.db"), "not a SQLite database").unwrap();
    let input_file = fixture.data_dir.join("response.json");
    std::fs::write(&input_file, COMPRESSIBLE_RESPONSE).unwrap();
    let baseline = fixture
        .command()
        .env("TOKENLESS_COMPRESSION_ENABLED", "1")
        .env("TOKENLESS_STATS_ENABLED", "0")
        .env("TOKENLESS_SLS_ENABLED", "0")
        .args(["compress-response", "--file"])
        .arg(&input_file)
        .output()
        .unwrap();
    assert!(baseline.status.success(), "{baseline:?}");

    let output = fixture
        .command()
        .env("TOKENLESS_COMPRESSION_ENABLED", "1")
        .env("TOKENLESS_STATS_ENABLED", "1")
        .env("TOKENLESS_SLS_ENABLED", "0")
        .args(["compress-response", "--file"])
        .arg(&input_file)
        .stderr(std::process::Stdio::from(dev_full()))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, baseline.stdout);
}

/// A compressible response: compacting indentation yields token
/// savings, so the stats/SLS recording tail of the command runs.
#[cfg(target_os = "linux")]
const COMPRESSIBLE_RESPONSE: &str = r#"{
    "data": "value",
    "debug": "diagnostic context",
    "trace": "request trace",
    "empty_field": ""
}"#;

#[test]
#[cfg(target_os = "linux")]
fn invalid_sls_path_warning_survives_an_unwritable_stderr() {
    let Some(fixture) = TempDataDir::new() else {
        return;
    };
    let home = get_home_dir();
    assert!(!home.is_empty());
    // A path under the home directory is outside the /var/log/ and /tmp/
    // allow-list, so the SLS writer rejects it and warns on stderr.
    let rejected = std::path::Path::new(&home).join(".tokenless-sls-rejected.jsonl");
    let output = fixture
        .command()
        .env("TOKENLESS_COMPRESSION_ENABLED", "1")
        .env("TOKENLESS_STATS_ENABLED", "1")
        .env("TOKENLESS_SLS_ENABLED", "1")
        .env("TOKENLESS_SLS_PATH", &rejected)
        .args(["compress-response"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::from(dev_full()))
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(COMPRESSIBLE_RESPONSE.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(
        output.status.success(),
        "rejecting TOKENLESS_SLS_PATH must not kill the compression (exit {})",
        output.status.code().unwrap_or(-1)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(result.get("data").is_some());
}

#[test]
#[cfg(target_os = "linux")]
fn sls_write_error_warning_survives_an_unwritable_stderr() {
    let Some(fixture) = TempDataDir::new() else {
        return;
    };
    // An existing directory passes SLS path validation and the existence
    // check, but cannot be opened for append: the write-error warning.
    let directory = tempfile::tempdir().unwrap();
    let output = fixture
        .command()
        .env("TOKENLESS_COMPRESSION_ENABLED", "1")
        .env("TOKENLESS_STATS_ENABLED", "1")
        .env("TOKENLESS_SLS_ENABLED", "1")
        .env("TOKENLESS_SLS_PATH", directory.path())
        .args(["compress-response"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::from(dev_full()))
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(COMPRESSIBLE_RESPONSE.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(
        output.status.success(),
        "the SLS write-error warning must not kill the compression (exit {})",
        output.status.code().unwrap_or(-1)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(result.get("data").is_some());
}

#[test]
#[cfg(target_os = "linux")]
fn corrupt_stats_warnings_survive_an_unwritable_stderr() {
    let Some(fixture) = TempDataDir::new() else {
        return;
    };
    let stats_db = fixture.data_dir.join("stats.db");
    std::fs::create_dir_all(&fixture.data_dir).unwrap();
    // One valid row through the real recorder (this also creates the
    // schema the CLI's open-time migration expects):
    StatsRecorder::new(&stats_db)
        .unwrap()
        .record(&StatsRecord::new(
            OperationType::CompressResponse,
            "cli".to_string(),
            10,
            5,
            6,
            3,
        ))
        .unwrap();
    // A timestamp that does not parse exercises the now()-substitution
    // warning; an unknown operation makes a row itself corrupt and
    // exercises the skip warning.
    let conn = rusqlite::Connection::open(&stats_db).unwrap();
    conn.execute("UPDATE stats SET timestamp = 'not-a-timestamp'", [])
        .unwrap();
    conn.execute(
        "INSERT INTO stats (
            timestamp, timestamp_ns, operation, agent_id,
            before_chars, before_tokens, after_chars, after_tokens
        ) VALUES ('2026-01-01T00:00:00+00:00', 0, 'not-an-operation', 'cli', 10, 5, 6, 3)",
        [],
    )
    .unwrap();
    drop(conn);

    let mut command = fixture.command();
    command
        .env("TOKENLESS_STATS_DB", &stats_db)
        .args(["stats", "list"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::from(dev_full()));
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "corrupt-row warnings must not kill `stats list` (exit {})",
        output.status.code().unwrap_or(-1)
    );
    let listing = String::from_utf8_lossy(&output.stdout);
    assert!(
        !listing.trim().is_empty(),
        "the recovered row must still be listed"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn stash_purge_warning_survives_an_unwritable_stderr() {
    let Some(fixture) = TempDataDir::new() else {
        return;
    };
    let stash_db = fixture.data_dir.join("stash-no-purge.db");
    std::fs::create_dir_all(&fixture.data_dir).unwrap();
    // A real stash table whose rows cannot be deleted (a BEFORE DELETE
    // trigger that aborts): the store opens cleanly — none of its
    // open-time statements delete — but the lazy-purge sweep on every
    // retrieve fails, while the retrieve SELECT keeps working.
    let payload = "stash-purge-payload";
    let hash = tokenless_ccr::compute_key(payload.as_bytes());
    let conn = rusqlite::Connection::open(&stash_db).unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE stash (
             hash TEXT PRIMARY KEY,
             payload TEXT NOT NULL,
             expires_at INTEGER NOT NULL,
             generation INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE stash_metadata(
             singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
             last_generation INTEGER NOT NULL CHECK(last_generation >= 0));
         CREATE TRIGGER block_lazy_purge BEFORE DELETE ON stash
         BEGIN SELECT RAISE(ABORT, 'lazy purge blocked'); END;
         INSERT INTO stash(hash, payload, expires_at, generation)
         VALUES ('{hash}', '{payload}', 9999999999, 0);
         INSERT INTO stash(hash, payload, expires_at, generation)
         VALUES ('bbbbbbbbbbbbbbbbbbbbbbbb', 'expired-payload', 0, 0);"
    ))
    .unwrap();
    drop(conn);

    let mut command = fixture.command();
    command
        .env("TOKENLESS_STATS_ENABLED", "0")
        .env("TOKENLESS_SLS_ENABLED", "0")
        .args(["retrieve", &hash])
        .arg("--stash-db")
        .arg(&stash_db)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::from(dev_full()));
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "the stash lazy-purge warning must not kill the retrieval (exit {})",
        output.status.code().unwrap_or(-1)
    );
    assert_eq!(output.stdout.as_slice(), payload.as_bytes());
}

// ─────────────────────────────────────────────────────────────────────
// A fatal error must keep the exit code `run()` chose (fd 2 → /dev/full)
// ─────────────────────────────────────────────────────────────────────
// The status is the machine-readable verdict a caller reads - 2 for
// rejected input, 1 for an operation failure - and the `Error:` line is
// only its detail. `eprintln!` panics when stderr cannot be written, and
// a panic on the fatal path replaced that verdict with 101.

/// Run `tokenless compress` on `input` with the given extra environment
/// and fd 2 bound to `stderr`, returning the exit code and stderr bytes.
#[cfg(target_os = "linux")]
fn compress_with_stderr(
    input: &[u8],
    env: &[(&str, &str)],
    stderr: std::process::Stdio,
) -> (Option<i32>, Vec<u8>) {
    let mut command = tokenless_bin();
    command
        .env("TOKENLESS_STATS_ENABLED", "0")
        .env("TOKENLESS_SLS_ENABLED", "0")
        .arg("compress")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(stderr);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(input)?;
            child.wait_with_output()
        })
        .unwrap();
    (output.status.code(), output.stderr)
}

/// Rejected input answers 2 whether or not the detail reaches the caller:
/// an unsupported protocol version carries no content to echo, so `run()`
/// rejects it before any compression work.
#[test]
#[cfg(target_os = "linux")]
fn rejected_input_keeps_its_exit_code_with_an_unwritable_stderr() {
    let input =
        br#"{"protocol_version":1,"content":"x","agent_id":"a","seam":"post_tool"}"#.as_slice();

    let (writable_code, writable_stderr) =
        compress_with_stderr(input, &[], std::process::Stdio::piped());
    assert_eq!(writable_code, Some(2), "documented rejected-input code");
    assert!(
        writable_stderr.starts_with(b"Error: "),
        "the detail is still printed when stderr accepts it: {}",
        String::from_utf8_lossy(&writable_stderr)
    );

    let (unwritable_code, _) =
        compress_with_stderr(input, &[], std::process::Stdio::from(dev_full()));
    assert_eq!(
        unwritable_code, writable_code,
        "an unwritable stderr must not replace the exit code with 101"
    );
}

/// An operation failure answers 1 on the same shape: a retrieve needs the
/// stash and a relative `TOKENLESS_DATA_DIR` is not a trusted state root.
#[test]
#[cfg(target_os = "linux")]
fn operation_failure_keeps_its_exit_code_with_an_unwritable_stderr() {
    let input = br#"{"protocol_version":2,"operation":"retrieve","attribution":{"agent_id":"a"},"input":{"hash_or_marker":"0123456789abcdef01234567","visible_markers":[]}}"#.as_slice();
    let env = [("TOKENLESS_DATA_DIR", "relative/data")];

    let (writable_code, writable_stderr) =
        compress_with_stderr(input, &env, std::process::Stdio::piped());
    assert_eq!(writable_code, Some(1), "documented operation-failure code");
    assert!(
        writable_stderr.starts_with(b"Error: "),
        "the detail is still printed when stderr accepts it: {}",
        String::from_utf8_lossy(&writable_stderr)
    );

    let (unwritable_code, _) =
        compress_with_stderr(input, &env, std::process::Stdio::from(dev_full()));
    assert_eq!(
        unwritable_code, writable_code,
        "an unwritable stderr must not replace the exit code with 101"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Success-path stdout writes must not panic when the write fails
// ─────────────────────────────────────────────────────────────────────
// `println!`/`print!` panic on a write error, so a command whose work was
// already done answered 101 with a panic message on stderr instead of the
// status it had earned - a full filesystem behind a redirect, or a consumer
// that stopped reading (`tokenless stats list | head -1`). The policy is the
// one ktuner settled for the same shape: a broken pipe means the consumer
// left while the work was finished, so the command ends gracefully and keeps
// its exit code; any other write error is an operation failure (code 1)
// reported through the CLI's normal error channel.

/// A child's stdout whose read end is already closed: every write answers
/// EPIPE, the deterministic shape of a consumer that stopped reading.
#[cfg(target_os = "linux")]
fn closed_pipe_stdout() -> std::process::Stdio {
    use std::os::unix::io::FromRawFd;
    let mut fds = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
    assert_eq!(unsafe { libc::close(fds[0]) }, 0, "close the read end");
    unsafe { std::process::Stdio::from_raw_fd(fds[1]) }
}

/// The listing command: a success path whose whole job is one stdout write.
#[test]
#[cfg(target_os = "linux")]
fn stats_list_ends_gracefully_when_the_consumer_stopped_reading() {
    let Some(fixture) = TempStatsDb::new() else {
        return;
    };
    let output = fixture
        .command()
        .env("TOKENLESS_SLS_ENABLED", "0")
        .args(["stats", "list"])
        .stdout(closed_pipe_stdout())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(0),
        "a broken pipe is a pipeline condition, not a failure: {stderr}"
    );
    assert!(!stderr.contains("panicked"), "{stderr}");
}

/// A full filesystem behind a redirect is not a pipeline condition: the
/// command reports an operation failure through the normal error channel.
#[test]
#[cfg(target_os = "linux")]
fn stats_list_maps_an_unwritable_stdout_to_an_operation_error() {
    let Some(fixture) = TempStatsDb::new() else {
        return;
    };
    let output = fixture
        .command()
        .env("TOKENLESS_SLS_ENABLED", "0")
        .args(["stats", "list"])
        .stdout(std::process::Stdio::from(dev_full()))
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.starts_with("Error: Failed to write output:"),
        "{stderr}"
    );
    assert!(!stderr.contains("panicked"), "{stderr}");
}

/// The payload path: a healthy run emits the compressed output byte-exact (the
/// writer appends no LF any more), and an unwritable stdout becomes an
/// operation error instead of a panic.
#[test]
#[cfg(target_os = "linux")]
fn compress_response_maps_an_unwritable_stdout_to_an_operation_error() {
    let Some(fixture) = TempDataDir::new() else {
        return;
    };
    let run = |stdout: std::process::Stdio| {
        fixture
            .command()
            .env("TOKENLESS_COMPRESSION_ENABLED", "1")
            .env("TOKENLESS_STATS_ENABLED", "0")
            .env("TOKENLESS_SLS_ENABLED", "0")
            .arg("compress-response")
            .stdin(std::process::Stdio::piped())
            .stdout(stdout)
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(COMPRESSIBLE_RESPONSE.as_bytes())?;
                child.wait_with_output()
            })
            .unwrap()
    };

    // Control: the healthy run wrote its payload through the same writer the
    // unwritable run below exercises. `compress-response` is byte-exact, so the
    // single trailing LF `println!` used to append is part of what this checks
    // is gone - the assertion follows the emitted bytes, not the old newline.
    let healthy = run(std::process::Stdio::piped());
    assert!(healthy.status.success(), "{healthy:?}");
    assert!(!healthy.stdout.is_empty(), "no payload was written");
    assert!(
        !healthy.stdout.ends_with(b"\n"),
        "the payload is emitted byte-exact, so the writer appends no LF: {}",
        String::from_utf8_lossy(&healthy.stdout)
    );

    let full = run(std::process::Stdio::from(dev_full()));
    let stderr = String::from_utf8_lossy(&full.stderr);
    assert_eq!(full.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.starts_with("Error: Failed to write output:"),
        "{stderr}"
    );
    assert!(!stderr.contains("panicked"), "{stderr}");
}

/// A prompt is a stdout write too: it must reach the terminal ahead of the
/// stdin read that follows it, and its failure must not leave the command
/// blocked on input nobody will answer.
#[test]
#[cfg(target_os = "linux")]
fn stats_clear_prompt_maps_an_unwritable_stdout_to_an_operation_error() {
    let Some(fixture) = TempStatsDb::new() else {
        return;
    };
    let output = fixture
        .command()
        .env("TOKENLESS_SLS_ENABLED", "0")
        .args(["stats", "clear"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(dev_full()))
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
}

/// The EPIPE shape of the same prompt, and the reason it is not covered by the
/// graceful broken-pipe policy: a consumer that stopped reading means the
/// destructive question was never asked, so the command must fail before it
/// waits on stdin - and before it clears anything. Swallowing the broken pipe
/// here reported an undelivered prompt as delivered: the command blocked on the
/// read, then cleared every record and exited 0 with an empty stderr.
#[test]
#[cfg(target_os = "linux")]
fn stats_clear_prompt_fails_without_clearing_when_the_consumer_stopped_reading() {
    let Some(fixture) = TempStatsDb::new() else {
        return;
    };
    let mut child = fixture
        .command()
        .env("TOKENLESS_SLS_ENABLED", "0")
        .args(["stats", "clear"])
        .stdin(std::process::Stdio::piped())
        .stdout(closed_pipe_stdout())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Park the answer in the pipe, so a command that still waits on stdin would
    // read it, clear the records and exit 0. A write error only means the child
    // had already given up on the prompt and exited.
    {
        use std::io::Write;
        let _ = child.stdin.as_mut().unwrap().write_all(b"y\n");
    }
    let output = child.wait_with_output().unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "an undelivered confirmation prompt must not be reported as success: {stderr}"
    );
    assert!(
        stderr.starts_with("Error: Failed to write output:"),
        "{stderr}"
    );
    assert!(stderr.contains("Broken pipe"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");

    // The destructive action never ran: the record the fixture wrote is still
    // in the database.
    let recorder = StatsRecorder::new(&fixture.path).unwrap();
    assert_eq!(
        recorder.count().unwrap(),
        1,
        "records were cleared behind a prompt nobody saw"
    );
    assert!(
        recorder.record_by_id(fixture.record_id).unwrap().is_some(),
        "the fixture record was cleared behind a prompt nobody saw"
    );
}

/// A response above the 500-character TOON minimum, so `compress-toon`
/// actually encodes it instead of taking the passthrough branch.
#[cfg(target_os = "linux")]
fn toon_encodable_response() -> String {
    let rows: Vec<String> = (0..24)
        .map(|index| format!(r#"{{"id":{index},"name":"row-{index}","note":"padding-{index}"}}"#))
        .collect();
    format!(r#"{{"rows":[{}]}}"#, rows.join(","))
}

/// The byte-exact writers share the policy: `compress-toon` reproduces its
/// payload verbatim, and by the time it is written the encoding is done, so a
/// consumer that left is a pipeline condition rather than a failed command.
#[test]
#[cfg(target_os = "linux")]
fn compress_toon_ends_gracefully_when_the_consumer_stopped_reading() {
    let Some(fixture) = TempDataDir::new() else {
        return;
    };
    let payload = toon_encodable_response();
    let run = |stdout: std::process::Stdio| {
        fixture
            .command()
            .env("TOKENLESS_COMPRESSION_ENABLED", "1")
            .env("TOKENLESS_STATS_ENABLED", "0")
            .env("TOKENLESS_SLS_ENABLED", "0")
            .arg("compress-toon")
            .stdin(std::process::Stdio::piped())
            .stdout(stdout)
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child.stdin.take().unwrap().write_all(payload.as_bytes())?;
                child.wait_with_output()
            })
            .unwrap()
    };

    // Control: the healthy run still emits the byte-exact body, with no LF the
    // detection contract does not expect.
    let healthy = run(std::process::Stdio::piped());
    let stderr = String::from_utf8_lossy(&healthy.stderr);
    assert!(healthy.status.success(), "{stderr}");
    assert!(!healthy.stdout.is_empty(), "no body was written");

    let closed = run(closed_pipe_stdout());
    let stderr = String::from_utf8_lossy(&closed.stderr);
    assert_eq!(
        closed.status.code(),
        Some(0),
        "a broken pipe is a pipeline condition, not a failure: {stderr}"
    );
    assert!(!stderr.contains("panicked"), "{stderr}");
}

/// A full filesystem behind a redirect stays an operation failure for the
/// byte-exact writer too - the bytes never reached the consumer.
#[test]
#[cfg(target_os = "linux")]
fn compress_toon_maps_an_unwritable_stdout_to_an_operation_error() {
    let Some(fixture) = TempDataDir::new() else {
        return;
    };
    let payload = toon_encodable_response();
    let output = fixture
        .command()
        .env("TOKENLESS_COMPRESSION_ENABLED", "1")
        .env("TOKENLESS_STATS_ENABLED", "0")
        .env("TOKENLESS_SLS_ENABLED", "0")
        .arg("compress-toon")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::from(dev_full()))
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(payload.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    // Not `starts_with`: compress-toon may note a passthrough or no-savings
    // disposition on stderr before the write error lands.
    assert!(
        stderr.contains("Error: Failed to write output:"),
        "{stderr}"
    );
    assert!(!stderr.contains("panicked"), "{stderr}");
}

/// `retrieve` had already read the payload out of the stash when the write
/// fails, so a consumer that left must not turn a completed restore into an
/// operation error.
#[test]
#[cfg(target_os = "linux")]
fn retrieve_ends_gracefully_when_the_consumer_stopped_reading() {
    let Some(fixture) = TempDataDir::new() else {
        return;
    };
    let stash_db = fixture.data_dir.join("stash.db");
    let stashed_string = format!("HELLO_RETRIEVE_PIPE_{}", "X".repeat(200));
    let original = format!("{{\"s\":\"{stashed_string}\"}}");

    let compressed = fixture
        .command()
        .env("TOKENLESS_COMPRESSION_ENABLED", "1")
        .env("TOKENLESS_STATS_ENABLED", "0")
        .args([
            "compress-response",
            "--truncate-strings-at",
            "120",
            "--stash-db",
        ])
        .arg(&stash_db)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(original.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(
        compressed.status.success(),
        "compress-response failed: {}",
        String::from_utf8_lossy(&compressed.stderr)
    );
    let compressed_text = String::from_utf8_lossy(&compressed.stdout);
    let marker = tokenless_ccr::extract_hash(&compressed_text)
        .expect("compressed output should contain a recovery instruction");

    let run = |stdout: std::process::Stdio| {
        fixture
            .command()
            .env("TOKENLESS_STATS_ENABLED", "0")
            .args(["retrieve", marker, "--stash-db"])
            .arg(&stash_db)
            .stdout(stdout)
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap()
    };

    // Control: the healthy restore is still byte-exact.
    let healthy = run(std::process::Stdio::piped());
    assert!(
        healthy.status.success(),
        "retrieve failed: {}",
        String::from_utf8_lossy(&healthy.stderr)
    );
    assert_eq!(healthy.stdout.as_slice(), stashed_string.as_bytes());

    let closed = run(closed_pipe_stdout());
    let stderr = String::from_utf8_lossy(&closed.stderr);
    assert_eq!(
        closed.status.code(),
        Some(0),
        "a broken pipe is a pipeline condition, not a failure: {stderr}"
    );
    assert!(!stderr.contains("panicked"), "{stderr}");
}

// ─────────────────────────────────────────────────────────────────────
// The CLI's own fail-soft notices must survive an unwritable stderr too
// ─────────────────────────────────────────────────────────────────────
// The tests above cover warnings raised inside the stats, SLS and stash
// libraries. The CLI prints notices of its own on paths that still
// succeed: a rejected database path falls back to the data directory, a
// stash that cannot be opened only disables recovery, and the no-savings,
// passthrough, dry-run and mode-mismatch notices describe a run whose
// result is already on stdout. A bare `eprintln!` panics when stderr
// cannot be written, so each of them replaced a successful exit 0 — and
// the output the caller was waiting for — with exit 101. Every test below
// first proves the notice really is on the exercised path (writable
// stderr), then re-runs the identical command with fd 2 on /dev/full and
// requires the status and the whole stdout to survive.

/// Run one command shape twice: with a writable stderr, and with fd 2 on
/// /dev/full. The writable run must succeed and print every notice in
/// `expected_notices` — otherwise the fixture no longer exercises the site
/// under test — and the /dev/full run must keep the exit status and the
/// complete stdout. `candidates` are tried in order as stdin, so a fixture
/// whose disposition sits on a token-estimate boundary can offer several
/// inputs and still fail loudly when none of them hits the path.
#[cfg(target_os = "linux")]
fn assert_notices_survive_unwritable_stderr(
    expected_notices: &[&str],
    candidates: &[String],
    configure: impl Fn(&mut Command),
) {
    let run = |stdin: &str, stderr: std::process::Stdio| -> std::process::Output {
        let mut command = tokenless_bin();
        configure(&mut command);
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(stderr)
            .spawn()
            .and_then(|mut child| {
                use std::io::Write as _;
                child.stdin.take().unwrap().write_all(stdin.as_bytes())?;
                child.wait_with_output()
            })
            .unwrap()
    };

    let mut selected = None;
    for candidate in candidates {
        let baseline = run(candidate, std::process::Stdio::piped());
        let stderr = String::from_utf8_lossy(&baseline.stderr);
        assert!(
            baseline.status.success(),
            "the fixture must succeed with a writable stderr (exit {}): {stderr}",
            baseline.status.code().unwrap_or(-1)
        );
        if expected_notices
            .iter()
            .all(|notice| stderr.contains(notice))
        {
            selected = Some((candidate.as_str(), baseline));
            break;
        }
    }
    let (stdin, baseline) = selected.unwrap_or_else(|| {
        panic!("no candidate stdin printed all of {expected_notices:?}; the fixture needs updating")
    });
    assert!(
        !baseline.stdout.is_empty(),
        "the fixture must produce a result on stdout"
    );

    let unwritable = run(stdin, std::process::Stdio::from(dev_full()));
    assert_eq!(
        unwritable.status.code(),
        Some(0),
        "an unwritable stderr must not turn the {expected_notices:?} notice into a failure"
    );
    assert_eq!(
        unwritable.stdout, baseline.stdout,
        "the successful result must survive the unwritable stderr"
    );
}

/// A tool schema whose over-cap `description` is truncated into the stash,
/// so the run writes a stash key. With the shortest name the compact
/// serialization lands on the same estimated token count as the input,
/// which takes the no-savings path and rolls that key back; longer names
/// save a token and keep it, hence the candidate list.
#[cfg(target_os = "linux")]
fn stashable_tool_schema(name: &str) -> String {
    serde_json::json!({
        "function": {
            "description": "A".repeat(257),
            "name": name,
            "parameters": {"properties": {}, "type": "object"}
        }
    })
    .to_string()
}

/// A clean, compact JSON object: `json_cleanup` has nothing to compact, so the
/// hook re-emits it byte-for-byte and reports no savings.
#[cfg(target_os = "linux")]
fn clean_json_object(keys: usize) -> String {
    const NAMES: [&str; 24] = [
        "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota", "kappa",
        "lambda", "mu", "nu", "xi", "omicron", "pi", "rho", "sigma", "tau", "upsilon", "phi",
        "chi", "psi", "omega",
    ];
    let fields: Vec<String> = (0..keys.min(NAMES.len()))
        .map(|index| format!("\"{}\":\"{}\"", NAMES[index], index + 1))
        .collect();
    format!("{{{}}}", fields.join(","))
}

/// A stash database whose rows cannot be inserted / cannot be deleted.
/// `SqliteStore` opens it cleanly — none of its open-time statements touch
/// the blocked event — so the failure lands on the fail-soft paths that the
/// schema compressor and the CLI only warn about.
#[cfg(target_os = "linux")]
fn create_stash_db_blocking(path: &std::path::Path, event: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let trigger = format!("block_stash_{}", event.to_lowercase());
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE stash (
             hash TEXT PRIMARY KEY,
             payload TEXT NOT NULL,
             expires_at INTEGER NOT NULL,
             generation INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE stash_metadata(
             singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
             last_generation INTEGER NOT NULL CHECK(last_generation >= 0));
         CREATE TRIGGER {trigger} BEFORE {event} ON stash
         BEGIN SELECT RAISE(ABORT, 'stash {event} blocked'); END;"
    ))
    .unwrap();
}

/// `compress-schema` emits the original input on the no-savings path.
#[test]
#[cfg(target_os = "linux")]
fn schema_no_savings_notice_survives_an_unwritable_stderr() {
    let Some(fixture) = TempDataDir::new() else {
        return;
    };
    assert_notices_survive_unwritable_stderr(
        &["schema compression did not reduce size"],
        &["{\"type\":\"object\"}".to_string()],
        |command| {
            fixture
                .configure(command)
                .env("TOKENLESS_COMPRESSION_ENABLED", "1")
                .env("TOKENLESS_STATS_ENABLED", "0")
                .env("TOKENLESS_SLS_ENABLED", "0")
                .args(["compress-schema", "--no-stash"]);
        },
    );
}

/// `compress-response` emits the original payload on the no-savings path.
#[test]
#[cfg(target_os = "linux")]
fn response_no_savings_notice_survives_an_unwritable_stderr() {
    let Some(fixture) = TempDataDir::new() else {
        return;
    };
    assert_notices_survive_unwritable_stderr(
        &["response compression did not reduce size"],
        &["{\"a\":1}".to_string()],
        |command| {
            fixture
                .configure(command)
                .env("TOKENLESS_COMPRESSION_ENABLED", "1")
                .env("TOKENLESS_STATS_ENABLED", "0")
                .env("TOKENLESS_SLS_ENABLED", "0")
                .args(["compress-response"]);
        },
    );
}

/// With compression disabled every run prints the dry-run notice, and a
/// payload with nothing to compact adds the no-savings one; both precede the
/// original payload on stdout.
#[test]
#[cfg(target_os = "linux")]
fn dry_run_notices_survive_an_unwritable_stderr() {
    let Some(fixture) = TempDataDir::new() else {
        return;
    };
    assert_notices_survive_unwritable_stderr(
        &[
            "response compression did not reduce size",
            "dry-run mode (compression disabled)",
        ],
        &["{\"a\":1}".to_string()],
        |command| {
            fixture
                .configure(command)
                .env("TOKENLESS_COMPRESSION_ENABLED", "0")
                .env("TOKENLESS_STATS_ENABLED", "0")
                .env("TOKENLESS_SLS_ENABLED", "0")
                .args(["compress-response"]);
        },
    );
}

/// `compress-toon` passes a payload under the TOON minimum through.
#[test]
#[cfg(target_os = "linux")]
fn toon_minimum_notice_survives_an_unwritable_stderr() {
    let Some(fixture) = TempDataDir::new() else {
        return;
    };
    assert_notices_survive_unwritable_stderr(
        &["TOON minimum"],
        &["{\"a\":1}".to_string()],
        |command| {
            fixture
                .configure(command)
                .env("TOKENLESS_COMPRESSION_ENABLED", "1")
                .env("TOKENLESS_STATS_ENABLED", "0")
                .env("TOKENLESS_SLS_ENABLED", "0")
                .args(["compress-toon"]);
        },
    );
}

/// The hook dispatch reports a PostTool run that saved nothing.
#[test]
#[cfg(target_os = "linux")]
fn post_tool_no_savings_notice_survives_an_unwritable_stderr() {
    let Some(fixture) = TempDataDir::new() else {
        return;
    };
    let candidates: Vec<String> = [20, 22, 24]
        .iter()
        .map(|keys| post_tool_request_json(&clean_json_object(*keys), false, "unwritable-stderr"))
        .collect();
    assert_notices_survive_unwritable_stderr(
        &["compression did not reduce size"],
        &candidates,
        |command| {
            fixture
                .configure(command)
                .env("TOKENLESS_COMPRESSION_ENABLED", "1")
                .env("TOKENLESS_STATS_ENABLED", "0")
                .env("TOKENLESS_SLS_ENABLED", "0")
                .args(["compress"]);
        },
    );
}

/// A `TOKENLESS_STATS_DB` outside the trusted roots is rejected and the
/// data directory is used instead — the recording still succeeds.
#[test]
#[cfg(target_os = "linux")]
fn rejected_stats_db_notice_survives_an_unwritable_stderr() {
    let Some(fixture) = TempDataDir::new_external() else {
        return;
    };
    // The fixture root is never under the real home (`TempDataDir::new_external`
    // refuses that case), so this path is outside both trusted roots.
    let rejected = fixture.root.join("rejected-stats.db");
    assert_notices_survive_unwritable_stderr(
        &["ignoring TOKENLESS_STATS_DB"],
        &[post_tool_request_json("ok", false, "rejected-stats-db")],
        |command| {
            fixture
                .configure(command)
                .env("TOKENLESS_COMPRESSION_ENABLED", "1")
                .env("TOKENLESS_STATS_ENABLED", "1")
                .env("TOKENLESS_SLS_ENABLED", "0")
                .env("TOKENLESS_STATS_DB", &rejected)
                .args(["compress"]);
        },
    );
}

/// A rejected `--stash-db` override warns and falls back to the data
/// directory, keeping the run recoverable.
#[test]
#[cfg(target_os = "linux")]
fn rejected_stash_db_override_notice_survives_an_unwritable_stderr() {
    let Some(fixture) = TempDataDir::new_external() else {
        return;
    };
    let rejected = fixture.root.join("rejected-stash.db");
    assert_notices_survive_unwritable_stderr(
        &["rejecting --stash-db"],
        &["{\"type\":\"object\"}".to_string()],
        |command| {
            fixture
                .configure(command)
                .env("TOKENLESS_COMPRESSION_ENABLED", "1")
                .env("TOKENLESS_STATS_ENABLED", "0")
                .env("TOKENLESS_SLS_ENABLED", "0")
                .args(["compress-schema", "--stash-db"])
                .arg(&rejected);
        },
    );
}

/// The same rejection through `TOKENLESS_STASH_DB`.
#[test]
#[cfg(target_os = "linux")]
fn rejected_stash_db_env_notice_survives_an_unwritable_stderr() {
    let Some(fixture) = TempDataDir::new_external() else {
        return;
    };
    let rejected = fixture.root.join("rejected-stash.db");
    assert_notices_survive_unwritable_stderr(
        &["ignoring TOKENLESS_STASH_DB"],
        &["{\"type\":\"object\"}".to_string()],
        |command| {
            fixture
                .configure(command)
                .env("TOKENLESS_COMPRESSION_ENABLED", "1")
                .env("TOKENLESS_STATS_ENABLED", "0")
                .env("TOKENLESS_SLS_ENABLED", "0")
                .env("TOKENLESS_STASH_DB", &rejected)
                .args(["compress-schema"]);
        },
    );
}

/// A stash database that cannot be opened only disables recovery; the
/// compression itself still produces output.
#[test]
#[cfg(target_os = "linux")]
fn stash_disabled_notice_survives_an_unwritable_stderr() {
    let Some(fixture) = TempDataDir::new() else {
        return;
    };
    let corrupt = fixture.data_dir.join("stash-corrupt.db");
    std::fs::create_dir_all(&fixture.data_dir).unwrap();
    std::fs::write(&corrupt, "not a SQLite database").unwrap();
    assert_notices_survive_unwritable_stderr(
        &["stash disabled"],
        &["x", "xx", "xxx", "xxxx"].map(stashable_tool_schema),
        |command| {
            fixture
                .configure(command)
                .env("TOKENLESS_COMPRESSION_ENABLED", "1")
                .env("TOKENLESS_STATS_ENABLED", "0")
                .env("TOKENLESS_SLS_ENABLED", "0")
                .args(["compress-schema", "--stash-db"])
                .arg(&corrupt);
        },
    );
}

/// Failed stash writes are counted and reported, but never fail the run.
#[test]
#[cfg(target_os = "linux")]
fn schema_stash_health_notice_survives_an_unwritable_stderr() {
    let Some(fixture) = TempDataDir::new() else {
        return;
    };
    let blocked = fixture.data_dir.join("stash-blocked-insert.db");
    create_stash_db_blocking(&blocked, "INSERT");
    assert_notices_survive_unwritable_stderr(
        &["stash operation(s) failed"],
        &["x", "xx", "xxx", "xxxx"].map(stashable_tool_schema),
        |command| {
            fixture
                .configure(command)
                .env("TOKENLESS_COMPRESSION_ENABLED", "1")
                .env("TOKENLESS_STATS_ENABLED", "0")
                .env("TOKENLESS_SLS_ENABLED", "0")
                .args(["compress-schema", "--stash-db"])
                .arg(&blocked);
        },
    );
}

/// The same report on the response path, where a truncated string is the
/// stash write that fails.
#[test]
#[cfg(target_os = "linux")]
fn response_stash_health_notice_survives_an_unwritable_stderr() {
    let Some(fixture) = TempDataDir::new() else {
        return;
    };
    let blocked = fixture.data_dir.join("stash-blocked-insert.db");
    create_stash_db_blocking(&blocked, "INSERT");
    let log: Vec<String> = (0..60)
        .map(|line| {
            format!(
                "2026-01-01T00:00:00Z INFO service.component line {line} \
                 processed request id=abc{line} latency={line}ms"
            )
        })
        .collect();
    let payload = serde_json::json!({"summary": "ok", "log": log.join("\n")}).to_string();
    assert_notices_survive_unwritable_stderr(
        &["stash operation(s) failed"],
        &[payload],
        |command| {
            fixture
                .configure(command)
                .env("TOKENLESS_COMPRESSION_ENABLED", "1")
                .env("TOKENLESS_STATS_ENABLED", "0")
                .env("TOKENLESS_SLS_ENABLED", "0")
                .args(["compress-response", "--stash-db"])
                .arg(&blocked);
        },
    );
}

/// The no-savings rollback delete failure is a diagnostic from
/// tokenless-schema: the failed delete is already counted in
/// `stash_errors`, which the CLI reports separately on the same run.
#[test]
#[cfg(target_os = "linux")]
fn stash_rollback_notice_survives_an_unwritable_stderr() {
    let Some(fixture) = TempDataDir::new() else {
        return;
    };
    let blocked = fixture.data_dir.join("stash-blocked-delete.db");
    create_stash_db_blocking(&blocked, "DELETE");
    assert_notices_survive_unwritable_stderr(
        &["did not reduce size", "rollback delete failed"],
        &["x", "xx", "xxx", "xxxx"].map(stashable_tool_schema),
        |command| {
            fixture
                .configure(command)
                .env("TOKENLESS_COMPRESSION_ENABLED", "1")
                .env("TOKENLESS_STATS_ENABLED", "0")
                .env("TOKENLESS_SLS_ENABLED", "0")
                .args(["compress-schema", "--stash-db"])
                .arg(&blocked);
        },
    );
}

/// The stats-compare mode hint is a sanity warning: the comparison itself
/// is still printed.
#[test]
#[cfg(target_os = "linux")]
fn mode_mismatch_notice_survives_an_unwritable_stderr() {
    let Some(fixture) = TempDataDir::new() else {
        return;
    };
    let stats_db = fixture.data_dir.join("stats.db");
    std::fs::create_dir_all(&fixture.data_dir).unwrap();
    // Both sides recorded as active, so the baseline side mismatches the
    // dry-run mode the comparison expects.
    let recorder = StatsRecorder::new(&stats_db).unwrap();
    for session in ["baseline-run", "tokenless-run"] {
        recorder
            .record(
                &StatsRecord::new(
                    OperationType::CompressResponse,
                    "integration-agent".to_string(),
                    1600,
                    400,
                    800,
                    200,
                )
                .with_session_id(session)
                .with_mode(CompressionMode::Active),
            )
            .unwrap();
    }
    drop(recorder);
    assert_notices_survive_unwritable_stderr(
        &["not in dry-run mode"],
        &["".to_string()],
        |command| {
            fixture
                .configure(command)
                .env("TOKENLESS_STATS_ENABLED", "0")
                .env("TOKENLESS_SLS_ENABLED", "0")
                .args([
                    "stats",
                    "summary",
                    "--compare",
                    "baseline-run",
                    "tokenless-run",
                ]);
        },
    );
}
