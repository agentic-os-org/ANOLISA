#[test]
fn msys_quoting_separates_backslash_pairs() {
    for (quoted, expected) in [
        (r"'\\server\share'", r"'\''\server\share'"),
        (r"'C:\user'\''s\\data'", r"'C:\user'\''s\''\data'"),
        (r"'C:\trailing\'", r"'C:\trailing\'"),
        (r"'a\\\b'", r"'a\''\\b'"),
        (r"'a\\\\b'", r"'a\''\\''\b'"),
    ] {
        assert_eq!(super::msys_harden_quoted(quoted), expected);
    }
}

/// Writes a batch file standing in for `tokenless.exe`: PowerShell calls
/// it with `rtk` first, as the anchored script does.
#[cfg(windows)]
fn fake_launcher(directory: &std::path::Path, body: &str) -> std::path::PathBuf {
    let launcher = directory.join("tokenless launcher.cmd");
    std::fs::write(&launcher, body).unwrap();
    launcher
}

#[cfg(windows)]
#[test]
fn powershell_binding_preserves_script_exit_and_restores_environment() {
    let directory = tempfile::tempdir().unwrap();
    // %1 is `rtk` and %2 the rewritten program, so %3 is the exit status.
    let launcher = fake_launcher(
        directory.path(),
        "@if not \"%1\"==\"rtk\" exit /b 99\r\n@echo %TOKENLESS_AGENT_ID%\r\n@exit /b %3\r\n",
    );
    let mut attribution = tokenless_protocol::Attribution::new("test-agent");
    attribution.session_id = Some("session 'quoted'".into());
    let shell =
        std::env::var("TOKENLESS_TEST_POWERSHELL").unwrap_or_else(|_| "powershell.exe".into());
    for (original, rewritten, status, last_exit, process_exit) in [
        ("git 7", "rtk git 7", "False", 7, 1),
        ("git 0", "rtk git 0", "True", 0, 0),
        (
            "git 7; Write-Output 'done'",
            "rtk git 7; Write-Output 'done'",
            "True",
            7,
            0,
        ),
    ] {
        let script = super::anchor_powershell_rtk(
            original,
            rewritten,
            &launcher,
            &attribution,
            std::path::Path::new("C:/data with spaces"),
        )
        .unwrap();
        // A user `rtk` alias must not capture the anchored invocation.
        let preamble = "$env:TOKENLESS_AGENT_ID='before'; Set-Alias rtk Write-Output; $ErrorActionPreference='Stop'; $Error.Clear();";
        let output = std::process::Command::new(&shell)
            .args(["-NoProfile", "-NonInteractive", "-Command", &format!(
                "{preamble} {script}; [Console]::Write(\"ok=$?;exit=$LASTEXITCODE;env=$env:TOKENLESS_AGENT_ID;alias=$((Get-Alias rtk).Definition);errors=$($Error.Count);preference=$ErrorActionPreference\")")])
            .output().unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty(), "{:?}", output.stderr);
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("test-agent"), "{stdout}");
        assert!(stdout.contains(&format!("ok={status};exit={last_exit};env=before;alias=Write-Output;errors=0;preference=Stop")), "{stdout}");

        let output = std::process::Command::new(&shell)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!("{preamble} {script}"),
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(process_exit), "{rewritten}");
        assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    }
}

#[cfg(windows)]
#[test]
fn powershell_binding_restores_environment_on_throw() {
    let directory = tempfile::tempdir().unwrap();
    let launcher = fake_launcher(directory.path(), "@exit /b 0\r\n");
    let script = super::anchor_powershell_rtk(
        "git status; throw 'expected-error'",
        "rtk git status; throw 'expected-error'",
        &launcher,
        &tokenless_protocol::Attribution::new("test-agent"),
        std::path::Path::new("C:/data"),
    )
    .unwrap();
    let shell =
        std::env::var("TOKENLESS_TEST_POWERSHELL").unwrap_or_else(|_| "powershell.exe".into());
    let output = std::process::Command::new(shell)
        .args(["-NoProfile", "-NonInteractive", "-Command", &format!(
            "$env:TOKENLESS_AGENT_ID='before'; try {{ {script} }} catch {{ [Console]::Write($_.Exception.Message) }}; [Console]::Write(\";env=$env:TOKENLESS_AGENT_ID\")")])
        .output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "expected-error;env=before"
    );
}

use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use tempfile::tempdir;
use tokenless_ccr::{InMemoryStore, StashError, StashStore, StashWrite, compute_key};
use tokenless_protocol::{
    BeforeModelCapabilities, ContentType, PostToolCapabilities, PreToolCapabilities,
};

use super::*;

// A local fixture, not the runtime defaults: these tests set each
// switch explicitly.
fn options() -> EntryOptions {
    EntryOptions {
        compression_enabled: true,
        search_path_sharing_enabled: true,
        diff_compression_enabled: false,
        html_extraction_enabled: false,
        browser_snapshot_enabled: false,
        mixed_content_enabled: false,
        stash_enabled: true,
        rtk_launcher: None,
        rtk_data_dir: Some(PathBuf::from("/tmp/tokenless-test")),
    }
}

/// Wraps [`InMemoryStore`] counting every trait-level touch — stash calls
/// as `writes`, retrieves as `reads`. A final row count alone cannot pin
/// a "never touched the store" contract: a stash that the pipeline later
/// rolled back also leaves `len() == 0`, so the counters, not the row
/// count, are what the dry-run and unavailable-path assertions must use.
#[derive(Default)]
struct ReadCountingStore {
    inner: InMemoryStore,
    reads: AtomicUsize,
    writes: AtomicUsize,
}

impl StashStore for ReadCountingStore {
    fn stash(&self, payload: &str) -> Result<StashWrite, StashError> {
        self.writes.fetch_add(1, Ordering::Relaxed);
        self.inner.stash(payload)
    }

    fn retrieve(&self, hash: &str) -> Result<Option<String>, StashError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.inner.retrieve(hash)
    }

    fn len(&self) -> usize {
        self.inner.len()
    }

    fn evict_expired(&self) -> Result<usize, StashError> {
        self.inner.evict_expired()
    }

    fn delete(&self, hash: &str, generation: u64) -> Result<bool, StashError> {
        self.inner.delete(hash, generation)
    }
}

struct FailingStore;

impl StashStore for FailingStore {
    fn stash(&self, _payload: &str) -> Result<StashWrite, StashError> {
        Err(StashError::Backend("simulated write failure".into()))
    }

    fn retrieve(&self, _hash: &str) -> Result<Option<String>, StashError> {
        Ok(None)
    }

    fn len(&self) -> usize {
        0
    }

    fn evict_expired(&self) -> Result<usize, StashError> {
        Ok(0)
    }

    fn delete(&self, _hash: &str, _generation: u64) -> Result<bool, StashError> {
        Ok(false)
    }
}

#[test]
fn retrieve_authorization_precedes_store_read() {
    let concrete = Arc::new(ReadCountingStore::default());
    let write = concrete.stash("byte-exact\n").unwrap();
    let store: Arc<dyn StashStore> = concrete.clone();
    let denied = RetrieveRequest {
        hash_or_marker: write.key.clone(),
        visible_markers: vec![],
    };
    assert!(matches!(
        retrieve_authorized_with_store(
            &denied,
            Some(&store),
            None,
            &Attribution::new("test"),
            "test"
        ),
        Err(RuntimeError::RetrieveUnauthorized { .. })
    ));
    assert_eq!(concrete.reads.load(Ordering::Relaxed), 0);
    let allowed = RetrieveRequest {
        hash_or_marker: write.key.clone(),
        visible_markers: vec![write.key.clone()],
    };
    let restored = retrieve_authorized_with_store(
        &allowed,
        Some(&store),
        None,
        &Attribution::new("test"),
        "test",
    )
    .unwrap();
    assert_eq!(restored.payload.as_bytes(), b"byte-exact\n");
    assert_eq!(concrete.reads.load(Ordering::Relaxed), 1);
}

#[test]
fn new_visible_references_authorize_only_the_current_static_tool() {
    let concrete = Arc::new(ReadCountingStore::default());
    let write = concrete.stash("恢复\n").unwrap();
    let store: Arc<dyn StashStore> = concrete.clone();
    let recovery = RecoveryMethod::tool("tenant_retrieve").unwrap();
    for (context, authorized) in [
        (write.key.clone(), false),
        (
            tokenless_ccr::recovery_instruction(
                &write.key,
                &RecoveryMethod::tool("other").unwrap(),
            ),
            false,
        ),
        (
            tokenless_ccr::recovery_instruction(&write.key, &recovery),
            true,
        ),
        (
            tokenless_ccr::recovery_instruction(&write.key, &RecoveryMethod::Shell),
            true,
        ),
        (tokenless_ccr::marker_for(&write.key), true),
    ] {
        let before = before_model_with_store(
            &BeforeModelRequest {
                tools: vec![],
                visible_context: json!({"messages": [context]}),
                capabilities: BeforeModelCapabilities {
                    replace_tools: false,
                    recovery: recovery.clone(),
                },
            },
            &options(),
            Some(&store),
        )
        .unwrap();
        let reads = concrete.reads.load(Ordering::Relaxed);
        let restored = retrieve_authorized_with_store(
            &RetrieveRequest {
                hash_or_marker: write.key.clone(),
                visible_markers: before.response.visible_markers,
            },
            Some(&store),
            None,
            &Attribution::new("test"),
            "test",
        );
        if authorized {
            assert_eq!(restored.unwrap().payload, "恢复\n");
            assert_eq!(concrete.reads.load(Ordering::Relaxed), reads + 1);
        } else {
            assert!(matches!(
                restored,
                Err(RuntimeError::RetrieveUnauthorized { .. })
            ));
            assert_eq!(concrete.reads.load(Ordering::Relaxed), reads);
        }
    }
}

#[test]
fn shell_recovery_does_not_enable_schema_stash() {
    let concrete = Arc::new(ReadCountingStore::default());
    let store: Arc<dyn StashStore> = concrete.clone();
    let request = BeforeModelRequest {
        tools: vec![json!({"name":"read", "description":"long description ".repeat(200)})],
        visible_context: json!([]),
        capabilities: BeforeModelCapabilities {
            replace_tools: true,
            recovery: RecoveryMethod::Shell,
        },
    };
    let result = before_model_with_store(&request, &options(), Some(&store)).unwrap();
    assert_eq!(result.response.tools, request.tools);
    assert!(result.response.visible_markers.is_empty());
    // Zero stash calls, not just a zero final row count: shell recovery
    // must never attach the stash, so nothing may be written and rolled
    // back either.
    assert_eq!(concrete.writes.load(Ordering::Relaxed), 0);
    assert_eq!(concrete.len(), 0);
}

#[test]
fn retrieve_records_attribution_only_after_authorization() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stats.db");
    let recorder = StatsRecorder::new(&database).unwrap();
    let store: Arc<dyn StashStore> = Arc::new(InMemoryStore::new());
    let write = store.stash("payload").unwrap();
    let attribution = Attribution {
        agent_id: "agent".into(),
        session_id: Some("session".into()),
        tool_use_id: Some("call".into()),
    };

    let denied = RetrieveRequest {
        hash_or_marker: write.key.clone(),
        visible_markers: Vec::new(),
    };
    assert!(
        retrieve_authorized_with_store(
            &denied,
            Some(&store),
            Some(&recorder),
            &attribution,
            "test"
        )
        .is_err()
    );
    assert_eq!(recorder.retrieve_totals().unwrap().hits, 0);

    let allowed = RetrieveRequest {
        hash_or_marker: write.key.clone(),
        visible_markers: vec![format!("<<tokenless:{}>>", write.key.to_ascii_uppercase())],
    };
    dispatch_with_store(
        &RequestEnvelope {
            attribution,
            request: Request::Retrieve(allowed),
        },
        &options(),
        Some(&store),
        Some(&recorder),
    )
    .unwrap();

    let connection = rusqlite::Connection::open(database).unwrap();
    let event: (String, String, String, String, String) = connection
        .query_row(
            "SELECT source, agent_id, session_id, tool_use_id, outcome FROM retrieve_events",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        event,
        (
            "cli".into(),
            "agent".into(),
            "session".into(),
            "call".into(),
            "hit".into()
        )
    );
}

/// Fails the test when PreTool reaches RTK for a command it must keep.
fn rtk_must_not_run(command: &str) -> rtk::RewriteOutcome {
    panic!("RTK must not see {command:?}")
}

fn bash_request(command: &str) -> PreToolRequest {
    PreToolRequest {
        tool_name: "Bash".into(),
        arguments: json!({"command": command, "timeout": 30}),
        command_field: "command".into(),
        capabilities: PreToolCapabilities {
            replace_arguments: true,
            block_and_suggest: false,
            post_tool_replacement: Default::default(),
        },
    }
}

#[test]
fn pre_tool_applies_rtk_rewrite_and_anchors_launcher() {
    let directory = tempdir().unwrap();
    let response = pre_tool_with_rewriter(
        &bash_request("git status"),
        &Attribution::new("test"),
        Some(Path::new("/opt/tokenless bin/tokenless")),
        Some(directory.path()),
        |command| {
            assert_eq!(command, "git status");
            rtk::RewriteOutcome::Allow("rtk git status".into())
        },
    )
    .unwrap();
    assert_eq!(response.action, PreToolAction::ReplaceArguments);
    assert_eq!(response.output_optimization, OutputOptimization::Rtk);
    assert_eq!(response.arguments["timeout"], 30);
    let command = response.arguments["command"].as_str().unwrap();
    assert!(
        command.starts_with("env TOKENLESS_AGENT_ID=test "),
        "{command}"
    );
    assert!(
        command.ends_with("'/opt/tokenless bin/tokenless' rtk git status"),
        "{command}"
    );
}

#[test]
fn search_modes_share_native_ownership_and_post_tool_recognition() {
    for command in [
        "grep -n 'foo|bar;baz' 'source file.py'",
        "cd /testbed && grep -n '__array_function__\\|NotImplemented' quantity.py",
        "rg --files",
        "rg -l match src",
        "grep -c match source.py",
        "rg -q match src",
        "rg --json match src",
        "rg --null -l match src",
        "git status | grep modified",
        "grep match source.py | sort",
        "echo start; rg -n match src",
        "echo start\nrg -n match src",
        "LANG=C command /usr/bin/grep match source.py",
        "env -u TEMP timeout -k 5 30 command rg match src",
        "nohup nice -n 5 /usr/bin/grep match source.py",
        "time -f '' rg match src",
        "uv run rg match src",
        "gr\\\nep match source.py",
    ] {
        let request = bash_request(command);
        let response = pre_tool_with_rewriter(
            &request,
            &Attribution::new("test"),
            None,
            None,
            rtk_must_not_run,
        )
        .unwrap();
        assert_eq!(response.arguments, request.arguments, "{command}");
        assert_eq!(response.action, PreToolAction::Passthrough);
        assert_eq!(response.output_optimization, OutputOptimization::None);
        assert!(is_search_command(command), "{command}");
    }
    for command in [
        "echo 'grep match src | rg x'",
        "printf '%s' \"rg match src; grep x\"",
        "rtk grep match src",
        "timeout --bogus 30 rg match src",
        "time -f rg echo match",
        "find . -type f",
        "head -40 source.py",
    ] {
        assert!(!is_search_command(command), "{command}");
    }
}

#[test]
fn pre_tool_preserves_native_source_previews() {
    for command in [
        "head -40 source.py",
        "head -n 40 'source file.py'",
        "head --lines=40 source.py",
        "tail -n 40 source.py",
        "tail --lines=40 source.py",
        "wc -l source.py; head -40 source.py",
        "cd /testbed && head -40 source.py",
        "cat source.py | head -40",
        "git diff | tail -40",
        "env LANG=C timeout 10 command /usr/bin/head -40 source.py",
        "nohup nice -n 5 tail -40 source.py",
        "uv run head -40 source.py",
        "he\\\nad -40 source.py",
    ] {
        let request = PreToolRequest {
            tool_name: "Bash".into(),
            arguments: json!({"command": command, "timeout": 30}),
            command_field: "command".into(),
            capabilities: PreToolCapabilities {
                replace_arguments: true,
                block_and_suggest: false,
                post_tool_replacement: Default::default(),
            },
        };
        let response = pre_tool_with_rewriter(
            &request,
            &Attribution::new("test"),
            None,
            None,
            rtk_must_not_run,
        )
        .unwrap_or_else(|error| panic!("{command:?}: {error}"));
        assert_eq!(response.action, PreToolAction::Passthrough, "{command}");
        assert_eq!(response.output_optimization, OutputOptimization::None);
        assert_eq!(response.arguments, request.arguments);
    }
    for command in [
        "echo 'head -40 source.py'",
        "echo 'rg tail source.py'",
        "time echo head",
        "timeout 10 rtk read source.py --max-lines 40",
        "rtk head -40 source.py",
        "rtk tail -40 source.py",
        "env LANG=C timeout 10 rtk head -40 source.py",
        "command rtk tail -40 source.py",
    ] {
        assert!(!keeps_command_native(command), "{command}");
    }
}

#[test]
fn pre_tool_keeps_native_find_commands_without_calling_rtk() {
    for command in [
        "find /workspace -name '*.json' 2>/dev/null",
        "find /workspace -path '*records*document*' 2>/dev/null",
        "find /workspace -name 'document*'; echo '---'; ls -la /workspace | head",
        "git status && /usr/bin/find ./data -type f",
        "env -i find ./data -print0 | xargs -0 cat",
        "LANG=C command find './project with spaces' -type f",
        "env -u TEMP /usr/bin/find . -type f",
        "noglob find . -name '*.json'",
        "nocorrect /usr/bin/find . -type f",
        "builtin find . -type f",
        "command exec noglob nocorrect find . -type f",
        "noglob env -u TEMP command find . -type f",
        "env -i nocorrect command exec /usr/bin/find . -type f",
    ] {
        let request = PreToolRequest {
            tool_name: "Bash".into(),
            arguments: json!({"command": command, "timeout": 30}),
            command_field: "command".into(),
            capabilities: PreToolCapabilities {
                replace_arguments: true,
                block_and_suggest: false,
                post_tool_replacement: Default::default(),
            },
        };
        // No launcher or data directory, and RTK panics if consulted.
        let response = pre_tool_with_rewriter(
            &request,
            &Attribution::new("test"),
            None,
            None,
            rtk_must_not_run,
        )
        .unwrap();
        assert_eq!(response.action, PreToolAction::Passthrough, "{command}");
        assert_eq!(response.output_optimization, OutputOptimization::None);
        assert_eq!(response.arguments, request.arguments);
    }
    for command in [
        "echo 'find . -name *.json'",
        "printf '%s' find",
        "echo 'rg find README.md'",
        "fd document",
        "echo 'rg --files'",
        "rtk find . -name '*.json'",
        "noglob echo find",
        "nocorrect printf '%s' 'find . -type f'",
        "builtin printf '%s' find",
        "command exec rtk find . -type f",
        "noglob git status",
        "nocorrect git diff",
        "command exec git log",
        "command env -i git status",
    ] {
        assert!(!keeps_command_native(command), "{command}");
    }
    for command in [
        "noglob cargo test",
        "nocorrect pytest -q",
        "command exec cargo test",
        "command env -i cargo test",
    ] {
        assert!(keeps_command_native(command), "{command}");
    }
}

#[test]
fn pre_tool_keeps_find_behind_rtk_process_wrappers() {
    for command in [
        "timeout 30 find . -type f",
        "timeout -k 5 -s TERM 30 find . -type f",
        "timeout --signal=TERM --kill-after=5 30 find . -type f",
        "timeout -sTERM -k5 --preserve-status --foreground -v --verbose 30 find . -type f",
        "timeout -- 30 find . -type f",
        "time find . -type f",
        "time -p -a -v --append --verbose --portability --quiet find . -type f",
        "time -f %e -o timing.log find . -type f",
        "time -f '' find . -type f",
        "timeout -s '' 30 find . -type f",
        "nice -n '' find . -type f",
        "time --format=%e --output=timing.log find . -type f",
        "time -f%e -otiming.log find . -type f",
        "time -- find . -type f",
        "nice -n 10 find . -type f",
        "nice -n10 find . -type f",
        "nice --adjustment=10 find . -type f",
        "nice -5 +10 find . -type f",
        "nice -- find . -type f",
        "nohup find . -type f",
        "nohup -- find . -type f",
        "/usr/bin/timeout 30 find . -type f",
        "/usr/bin/time -p /usr/bin/find . -type f",
        "/usr/bin/nice -n10 /usr/bin/find . -type f",
        "/usr/bin/nohup /usr/bin/find . -type f",
        "env FOO=1 timeout 20 find . -type f",
        "nohup nice -n 5 timeout 30 find . -type f",
        "noglob env -u TEMP timeout 30 command find . -type f",
        "uv run find . -type f",
        "timeout 30 uv run find . -type f",
        "uv run nohup nice -n 5 find . -type f",
        "\\\n  find . -type f",
        "command \\\n  find . -type f",
        "noglob \\\n  find . -type f",
        "env \\\n  FOO=1 \\\n  find . -type f",
        "timeout 30 \\\n  find . -type f",
        "fi\\\nnd . -type f",
    ] {
        let response = pre_tool_with_rewriter(
            &PreToolRequest {
                tool_name: "Bash".into(),
                arguments: json!({"command": command}),
                command_field: "command".into(),
                capabilities: PreToolCapabilities {
                    replace_arguments: true,
                    block_and_suggest: false,
                    post_tool_replacement: Default::default(),
                },
            },
            &Attribution::new("test"),
            None,
            None,
            rtk_must_not_run,
        )
        .unwrap_or_else(|error| panic!("{command:?}: {error}"));
        assert_eq!(response.action, PreToolAction::Passthrough, "{command}");
        assert_eq!(response.output_optimization, OutputOptimization::None);
        assert_eq!(response.arguments["command"], command);
    }
    for command in [
        "timeout 30 echo 'rg find README.md'",
        "time echo find",
        "nohup rtk find . -type f",
        "timeout 30 rtk find . -type f",
        "timeout --bogus 30 find .",
        "time --bogus find .",
        "nice --bogus find .",
        "nohup --bogus find .",
        "timeout 30 git status",
        "nice -n 10 git log",
        "TIMEOUT 30 find .",
        "timeout",
        "timeout -s",
        "timeout -s TERM",
        "time -f",
        "nice -n",
        "timeout -s find 30 echo find",
        "timeout -s rtk 30 find .",
        "time -f find echo find",
        "uv run echo find",
        "uv run rtk find .",
        "'' find . -type f",
        "command '' find . -type f",
        "timeout 30 '' find . -type f",
    ] {
        assert!(!keeps_command_native(command), "{command}");
    }
    for command in ["timeout 30 cargo build", "nice -n 10 pytest -q"] {
        assert!(keeps_command_native(command), "{command}");
    }
}

#[test]
fn pre_tool_ownership_folds_shell_line_continuations() {
    for command in [
        "\\\n  find . -type f",
        "command \\\n  find . -type f",
        "noglob \\\n  find . -type f",
        "env \\\n  FOO=1 \\\n  find . -type f",
        "timeout 30 \\\n  find . -type f",
        "fi\\\nnd . -type f",
        "env \\\n  RUSTFLAGS=-C \\\n  cargo test",
    ] {
        assert!(keeps_command_native(command), "{command:?}");
    }
    for command in [
        "echo 'command \\\n find . -type f'",
        "printf '%s' \"noglob \\\n find . -type f\"",
    ] {
        assert!(!keeps_command_native(command), "{command:?}");
    }
}

#[test]
fn pre_tool_reserves_supported_build_and_test_commands_for_post_tool() {
    let commands = [
        "cargo test --workspace",
        "RUST_BACKTRACE=1 /usr/bin/cargo check",
        "cd crate && cargo clippy",
        "python3.12 -m pytest tests/",
        "uv run python -m pytest -q",
        "npm test",
        "npm run --silent test:unit",
        "pnpm exec jest --runInBand",
        "npx jest",
        "go test ./... 2>&1 | tail -40",
        "make -j8",
    ];
    for command in commands {
        assert!(
            keeps_command_native(command),
            "expected BuildLog ownership for {command:?}"
        );
    }

    for command in [
        "cargo metadata",
        "cargo fmt --all",
        "go env",
        "npm run lint",
        "cat build.log",
        "echo 'cargo test'",
        "echo 'rg pytest README.md'",
    ] {
        assert!(
            !keeps_command_native(command),
            "unexpected BuildLog ownership for {command:?}"
        );
    }
}

#[test]
fn pre_tool_owns_build_commands_behind_env_flags_and_toolchains() {
    for command in [
        "env -i cargo test",
        "env -u RUSTC cargo build",
        "env --ignore-environment cargo check",
        "/usr/bin/env cargo test",
        "env -i make",
        "env -i go test ./...",
        "env -i npm test",
        "cargo +nightly test",
        "cargo +stable build",
        "cargo +1.84 check",
        "cargo +toolchain-with-dashes clippy",
        "cargo test --workspace",
        "RUST_BACKTRACE=1 cargo check",
        "env CARGO_X=1 cargo test",
        "command cargo test",
        "python3.12 -m pytest",
        "make -j8",
    ] {
        assert!(
            keeps_command_native(command),
            "expected BuildLog ownership for {command:?}"
        );
    }

    for command in [
        "cargo +nightly fmt",
        "env -i cat build.log",
        "env -u RUSTC echo 'grep error log'",
    ] {
        assert!(
            !keeps_command_native(command),
            "unexpected BuildLog ownership for {command:?}"
        );
    }
}

fn pre_tool_with_rtk(
    request: &PreToolRequest,
    attribution: &Attribution,
    rtk_launcher: &Path,
    data_dir: &Path,
) -> Result<PreToolResponse, RuntimeError> {
    pre_tool_with_rewriter(
        request,
        attribution,
        Some(rtk_launcher),
        Some(data_dir),
        |command| rtk_rewrite(Some(rtk_launcher), command),
    )
}

#[test]
fn pre_tool_build_log_owner_behind_env_flag_beats_rtk_rewriting() {
    let directory = tempdir().unwrap();
    let response = pre_tool_with_rtk(
        &bash_request("env -i cargo test"),
        &Attribution::new("test"),
        Path::new("/opt/tokenless/tokenless"),
        directory.path(),
    )
    .unwrap();
    assert_eq!(response.action, PreToolAction::Passthrough);
    assert_eq!(response.output_optimization, OutputOptimization::None);
    assert_eq!(
        response.arguments["command"].as_str().unwrap(),
        "env -i cargo test"
    );
}

#[test]
fn pre_tool_passes_through_once_the_launcher_leaves_its_path() {
    let directory = tempdir().unwrap();
    let launcher = directory.path().join("tokenless");
    std::fs::write(&launcher, "").unwrap();
    let rewrite = |launcher: &Path| {
        pre_tool_with_rtk(
            &bash_request("git status"),
            &Attribution::new("test"),
            launcher,
            directory.path(),
        )
        .unwrap()
    };
    assert_eq!(rewrite(&launcher).action, PreToolAction::ReplaceArguments);

    // `/proc/self/exe` after the running executable was replaced.
    let replaced = directory.path().join("tokenless (deleted)");
    let response = rewrite(&replaced);
    assert_eq!(response.action, PreToolAction::Passthrough);
    assert_eq!(response.output_optimization, OutputOptimization::None);
    assert_eq!(response.arguments["command"], "git status");
}

#[test]
fn pre_tool_build_log_owner_does_not_require_rtk() {
    let request = PreToolRequest {
        tool_name: "Bash".into(),
        arguments: json!({"command": "cargo test --workspace"}),
        command_field: "command".into(),
        capabilities: PreToolCapabilities {
            replace_arguments: true,
            block_and_suggest: false,
            post_tool_replacement: Default::default(),
        },
    };
    let response = pre_tool_with_rewriter(
        &request,
        &Attribution::new("test"),
        None,
        None,
        rtk_must_not_run,
    )
    .unwrap();
    assert_eq!(response.action, PreToolAction::Passthrough);
    assert_eq!(response.output_optimization, OutputOptimization::None);
    assert_eq!(response.arguments, request.arguments);
}

#[test]
fn pre_tool_anchor_preserves_quoted_arguments_and_handles_subshells() {
    let path = Path::new("/opt/tokenless/tokenless");
    let data_dir = Path::new("/tenant/tokenless");
    let attribution = Attribution {
        agent_id: "agent".into(),
        session_id: Some("session".into()),
        tool_use_id: Some("call".into()),
    };
    let prefix = "env TOKENLESS_AGENT_ID=agent TOKENLESS_SESSION_ID=session TOKENLESS_TOOL_USE_ID=call TOKENLESS_DATA_DIR=/tenant/tokenless /opt/tokenless/tokenless rtk";
    assert_eq!(
        anchor_rtk_prefix(
            "grep -E 'foo | rtk bar' src && git status",
            "rtk grep -E 'foo | rtk bar' src && rtk git status",
            path,
            &attribution,
            data_dir,
        ),
        format!("{prefix} grep -E 'foo | rtk bar' src && {prefix} git status")
    );
    assert_eq!(
        anchor_rtk_prefix(
            "echo $(git status)",
            "echo $(rtk git status)",
            path,
            &attribution,
            data_dir,
        ),
        format!("echo $({prefix} git status)")
    );
    assert_eq!(
        anchor_rtk_prefix(
            "echo `git status`",
            "echo `rtk git status`",
            path,
            &attribution,
            data_dir,
        ),
        "echo `rtk git status`"
    );
    assert_eq!(
        anchor_rtk_prefix(
            "sudo git status",
            "sudo rtk git status",
            path,
            &attribution,
            data_dir,
        ),
        format!("sudo {prefix} git status")
    );
    assert_eq!(
        anchor_rtk_prefix(
            "RUST_BACKTRACE=1 cargo test",
            "RUST_BACKTRACE=1 rtk cargo test",
            path,
            &attribution,
            data_dir,
        ),
        format!("RUST_BACKTRACE=1 {prefix} cargo test")
    );
    assert_eq!(
        anchor_rtk_prefix(
            "sudo noglob git status",
            "sudo noglob rtk git status",
            path,
            &attribution,
            data_dir,
        ),
        format!("sudo noglob {prefix} git status")
    );
    assert_eq!(
        anchor_rtk_prefix(
            "shadowenv exec -- git status",
            "shadowenv exec -- rtk git status",
            path,
            &attribution,
            data_dir,
        ),
        format!("shadowenv exec -- {prefix} git status")
    );
    assert_eq!(
        anchor_rtk_prefix(
            "git status | grep rtk",
            "rtk git status | grep rtk",
            path,
            &attribution,
            data_dir,
        ),
        format!("{prefix} git status | grep rtk")
    );
    assert_eq!(
        anchor_rtk_prefix(
            "docker exec rtk git status",
            "docker exec rtk rtk git status",
            path,
            &attribution,
            data_dir,
        ),
        format!("docker exec rtk {prefix} git status")
    );
    assert_eq!(
        anchor_rtk_prefix("rg error", "rtk rg error", path, &attribution, data_dir),
        format!("{prefix} rg error")
    );
}

#[test]
fn rtk_receipts_cover_all_wrappers_or_keep_conservative_ownership() {
    let attribution = Attribution::new("test");
    let path = Path::new("/opt/tokenless/tokenless");
    let anchored = anchor_rtk_prefix_with_outcomes(
        "git log && git show",
        "rtk git log && rtk git show",
        path,
        &attribution,
        Path::new("/tmp/state"),
        |count, render| {
            assert_eq!(count, 2);
            Some(render("generation"))
        },
    );
    assert!(anchored.contains("TOKENLESS_RTK_ORDINAL=0"));
    assert!(anchored.contains("TOKENLESS_RTK_ORDINAL=1"));
    for original in [
        "rtk git log && git show",
        "/usr/local/bin/rtk git log && git show",
        "echo \"$(rtk git log)\" && git show",
        "echo `git log` && git show",
    ] {
        anchor_rtk_prefix_with_outcomes(
            original,
            &original.replace("git show", "rtk git show"),
            path,
            &attribution,
            Path::new("/tmp/state"),
            |count, _| {
                assert_eq!(count, 0);
                None
            },
        );
    }
}

#[test]
fn pre_tool_no_op_does_not_require_rtk() {
    let requests = [
        PreToolRequest {
            tool_name: "Read".into(),
            arguments: json!({"path": "README.md"}),
            command_field: "command".into(),
            capabilities: PreToolCapabilities {
                replace_arguments: true,
                block_and_suggest: false,
                post_tool_replacement: Default::default(),
            },
        },
        PreToolRequest {
            tool_name: "Bash".into(),
            arguments: Value::String("not an object".into()),
            command_field: "command".into(),
            capabilities: PreToolCapabilities {
                replace_arguments: true,
                block_and_suggest: false,
                post_tool_replacement: Default::default(),
            },
        },
        PreToolRequest {
            tool_name: "Bash".into(),
            arguments: json!({"command": "git status"}),
            command_field: "command".into(),
            capabilities: PreToolCapabilities {
                replace_arguments: false,
                block_and_suggest: false,
                post_tool_replacement: Default::default(),
            },
        },
    ];
    for request in requests {
        let outcome = dispatch_with_store(
            &RequestEnvelope {
                attribution: Attribution::new("test"),
                request: Request::PreTool(request.clone()),
            },
            &options(),
            None,
            None,
        )
        .unwrap();
        let Response::PreTool(response) = outcome.response.response else {
            unreachable!("the request operation fixes the response variant")
        };
        assert_eq!(response.action, PreToolAction::Passthrough);
        assert_eq!(response.arguments, request.arguments);
    }

    let applicable = PreToolRequest {
        tool_name: "Bash".into(),
        arguments: json!({"command": "git status"}),
        command_field: "command".into(),
        capabilities: PreToolCapabilities {
            replace_arguments: true,
            block_and_suggest: false,
            post_tool_replacement: Default::default(),
        },
    };
    assert!(matches!(
        pre_tool_with_rewriter(
            &applicable,
            &Attribution::new("test"),
            None,
            None,
            rtk_must_not_run
        ),
        Err(RuntimeError::RtkLauncherUnavailable)
    ));
}

#[test]
fn pre_tool_honors_rtk_outcomes_and_preserves_arguments() {
    let directory = tempdir().unwrap();
    let request = bash_request("git status");
    let launcher = Path::new("/opt/tokenless/tokenless");
    for outcome in [
        rtk::RewriteOutcome::Passthrough,
        rtk::RewriteOutcome::Deny,
        rtk::RewriteOutcome::Allow(String::new()),
        rtk::RewriteOutcome::Allow("git status".into()),
    ] {
        let response = pre_tool_with_rewriter(
            &request,
            &Attribution::new("test"),
            Some(launcher),
            Some(directory.path()),
            |_| outcome,
        )
        .unwrap();
        assert_eq!(response.action, PreToolAction::Passthrough);
        assert_eq!(response.output_optimization, OutputOptimization::None);
        assert_eq!(response.arguments, request.arguments);
    }

    let response = pre_tool_with_rewriter(
        &request,
        &Attribution::new("test"),
        Some(launcher),
        Some(directory.path()),
        |_| rtk::RewriteOutcome::Ask("optimized command".into()),
    )
    .unwrap();
    assert_eq!(response.action, PreToolAction::ReplaceArguments);
    assert_eq!(response.output_optimization, OutputOptimization::Rtk);
    assert_eq!(response.arguments["command"], "optimized command");
    assert_eq!(response.arguments["timeout"], 30);
}

#[test]
fn pre_tool_carries_attribution_into_the_rewritten_command() {
    let directory = tempdir().unwrap();
    let request = PreToolRequest {
        tool_name: "Bash".into(),
        arguments: json!({"command": "git status"}),
        command_field: "command".into(),
        capabilities: PreToolCapabilities {
            replace_arguments: false,
            block_and_suggest: true,
            post_tool_replacement: Default::default(),
        },
    };
    let attribution = Attribution {
        agent_id: "agent".into(),
        session_id: Some("session".into()),
        tool_use_id: Some("call".into()),
    };
    let response = pre_tool_with_rewriter(
        &request,
        &attribution,
        Some(Path::new("/opt/tokenless/tokenless")),
        Some(directory.path()),
        |_| rtk::RewriteOutcome::Allow("rtk git status".into()),
    )
    .unwrap();
    assert_eq!(response.action, PreToolAction::BlockAndSuggest);
    let command = response.arguments["command"].as_str().unwrap();
    assert!(
        command.starts_with(&format!(
            "env TOKENLESS_AGENT_ID=agent TOKENLESS_SESSION_ID=session TOKENLESS_TOOL_USE_ID=call TOKENLESS_DATA_DIR={} TOKENLESS_RTK_RUN_ID=",
            shell_quote(&directory.path().to_string_lossy())
        )),
        "{command}"
    );
    assert!(
        command.ends_with(" TOKENLESS_RTK_ORDINAL=0 /opt/tokenless/tokenless rtk git status"),
        "{command}"
    );
}

#[test]
fn powershell_anchor_calls_the_launcher_only_at_command_starts() {
    let launcher = Path::new("C:/Program Files/Tokenless/tokenless.exe");
    let attribution = Attribution::new("test");
    let data_dir = Path::new("C:/data");
    let invocation = "& 'C:/Program Files/Tokenless/tokenless.exe' rtk";
    for (original, rewritten, expected) in [
        (
            "git status",
            "rtk git status",
            format!("{invocation} git status"),
        ),
        (
            "git log | Select-Object -First 2; git status",
            "rtk git log | Select-Object -First 2; rtk git status",
            format!("{invocation} git log | Select-Object -First 2; {invocation} git status"),
        ),
        (
            "if ($ok) { git diff } && (ls)",
            "if ($ok) { rtk git diff } && (rtk ls)",
            format!("if ($ok) {{ {invocation} git diff }} && ({invocation} ls)"),
        ),
        (
            "Write-Output 'it''s rtk'; git status",
            "Write-Output 'it''s rtk'; rtk git status",
            format!("Write-Output 'it''s rtk'; {invocation} git status"),
        ),
    ] {
        let script = anchor_powershell_rtk(original, rewritten, launcher, &attribution, data_dir)
            .unwrap_or_else(|| panic!("{rewritten:?} was not anchored"));
        assert!(script.contains(&format!("{expected}\n}}")), "{script}");
        assert!(!script.contains("Set-Alias"), "{script}");
    }
    for (original, rewritten) in [
        // A wrapper's argument is not a command PowerShell would resolve.
        ("timeout 30 git status", "timeout 30 rtk git status"),
        // After a background `&` the inserted call operator would double up.
        ("Start-Job {} & git status", "Start-Job {} & rtk git status"),
        // A PowerShell backtick escape hides the insertion from the scanner.
        (
            "Write-Output `\"; git status",
            "Write-Output `\"; rtk git status",
        ),
        ("git status", "git status"),
    ] {
        assert!(
            anchor_powershell_rtk(original, rewritten, launcher, &attribution, data_dir).is_none(),
            "{rewritten:?}"
        );
    }
}

#[test]
fn before_model_without_retrieve_capability_keeps_schema_unchanged() {
    let request = BeforeModelRequest {
        tools: vec![json!({
            "type": "function",
            "function": {
                "name": "read",
                "description": "long description ".repeat(200),
                "parameters": {"type": "object", "properties": {}}
            }
        })],
        visible_context: json!({"messages": []}),
        capabilities: BeforeModelCapabilities {
            replace_tools: true,
            recovery: tokenless_protocol::RecoveryMethod::None,
        },
    };
    let outcome = before_model_with_store(&request, &options(), None).unwrap();
    assert_eq!(outcome.response.tools, request.tools);
    assert!(outcome.response.visible_markers.is_empty());
    assert_eq!(
        outcome.stats.disposition,
        Disposition::RecoverabilityUnavailable
    );
    assert!(outcome.stats.applied_operations.is_empty());
}

#[test]
fn before_model_returns_sorted_markers_with_recovery_available() {
    let store: Arc<dyn StashStore> = Arc::new(InMemoryStore::new());
    let request = BeforeModelRequest {
        tools: vec![json!({
            "type": "function",
            "function": {
                "name": "read",
                "description": "long description ".repeat(200),
                "parameters": {"type": "object", "properties": {}}
            }
        })],
        visible_context: json!({
            "messages": [
                "<<tokenless:ABCDEF0123456789ABCDEF01>>",
                "<<tokenless:abcdef0123456789abcdef01>>"
            ]
        }),
        capabilities: BeforeModelCapabilities {
            replace_tools: true,
            recovery: RecoveryMethod::tool("tokenless_retrieve").unwrap(),
        },
    };
    let outcome = before_model_with_store(&request, &options(), Some(&store)).unwrap();
    assert!(
        outcome
            .response
            .visible_markers
            .windows(2)
            .all(|pair| pair[0] < pair[1])
    );
    assert_eq!(
        outcome
            .response
            .visible_markers
            .iter()
            .filter(|hash| *hash == "abcdef0123456789abcdef01")
            .count(),
        1
    );
    assert!(!outcome.artifact_keys.is_empty());
}

/// Delegates to InMemoryStore but fails the SECOND stash call — the
/// mid-run partial-failure shape (first tool's row already written).
struct FailAfterFirstStore {
    inner: InMemoryStore,
    stashes: AtomicUsize,
}

impl StashStore for FailAfterFirstStore {
    fn stash(&self, payload: &str) -> Result<StashWrite, StashError> {
        if self.stashes.fetch_add(1, Ordering::Relaxed) >= 1 {
            return Err(StashError::Backend("second write fails".into()));
        }
        self.inner.stash(payload)
    }

    fn retrieve(&self, hash: &str) -> Result<Option<String>, StashError> {
        self.inner.retrieve(hash)
    }

    fn len(&self) -> usize {
        self.inner.len()
    }

    fn evict_expired(&self) -> Result<usize, StashError> {
        self.inner.evict_expired()
    }

    fn delete(&self, hash: &str, generation: u64) -> Result<bool, StashError> {
        self.inner.delete(hash, generation)
    }
}

fn long_tool(name: &str) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": format!("long {name} description ").repeat(100),
            "parameters": {"type": "object", "properties": {}}
        }
    })
}

#[test]
fn before_model_stash_write_failure_rolls_back_and_surfaces_the_error() {
    // Two tools whose descriptions both truncate; the store fails the
    // second write. The first tool's row must be rolled back (it has no
    // marker) and the error surfaced — a refactor that returns the error
    // before the rollback leaks one row.
    let concrete = Arc::new(FailAfterFirstStore {
        inner: InMemoryStore::new(),
        stashes: AtomicUsize::new(0),
    });
    let store: Arc<dyn StashStore> = concrete.clone();
    let request = BeforeModelRequest {
        tools: vec![long_tool("read"), long_tool("write")],
        visible_context: json!({"messages": []}),
        capabilities: BeforeModelCapabilities {
            replace_tools: true,
            recovery: RecoveryMethod::tool("tenant_retrieve").unwrap(),
        },
    };
    let result = before_model_with_store(&request, &options(), Some(&store));
    match result {
        Err(RuntimeError::StashWrite { count }) => assert_eq!(count, 1),
        Err(other) => panic!("expected StashWrite, got another error: {other}"),
        Ok(_) => panic!("expected StashWrite, got Ok"),
    }
    assert_eq!(
        concrete.stashes.load(Ordering::Relaxed),
        2,
        "one stash call per truncated description: the first succeeds, the second fails"
    );
    assert_eq!(
        concrete.len(),
        0,
        "the first tool's already-written row must be rolled back"
    );
}

#[test]
fn before_model_without_a_store_emits_original_tools() {
    // Tool recovery declared but no store available (the production
    // shape when the stash DB cannot be opened): the host must receive
    // the ORIGINAL untruncated tools, never a lossy schema.
    let request = BeforeModelRequest {
        tools: vec![long_tool("read"), long_tool("write")],
        visible_context: json!({"messages": []}),
        capabilities: BeforeModelCapabilities {
            replace_tools: true,
            recovery: RecoveryMethod::tool("tenant_retrieve").unwrap(),
        },
    };
    let outcome = before_model_with_store(&request, &options(), None).unwrap();
    assert_eq!(
        outcome.stats.disposition,
        Disposition::RecoverabilityUnavailable
    );
    assert_eq!(
        serde_json::to_string(&outcome.response.tools).unwrap(),
        serde_json::to_string(&request.tools).unwrap(),
        "no store means never emit lossy schemas"
    );
    assert!(outcome.artifact_keys.is_empty());
    assert_eq!(outcome.stats.recoverability, Recoverability::Lossless);
}

#[test]
fn before_model_dry_run_emits_original_tools_and_never_touches_the_store() {
    // compression_enabled=false: the measured candidate is reported but
    // the caller's store is never written.
    let concrete = Arc::new(ReadCountingStore::default());
    let store: Arc<dyn StashStore> = concrete.clone();
    let mut opts = options();
    opts.compression_enabled = false;
    let request = BeforeModelRequest {
        tools: vec![long_tool("read")],
        visible_context: json!({"messages": []}),
        capabilities: BeforeModelCapabilities {
            replace_tools: true,
            recovery: RecoveryMethod::tool("tenant_retrieve").unwrap(),
        },
    };
    let outcome = before_model_with_store(&request, &opts, Some(&store)).unwrap();
    assert_eq!(outcome.stats.disposition, Disposition::DryRun);
    assert_eq!(
        serde_json::to_string(&outcome.response.tools).unwrap(),
        serde_json::to_string(&request.tools).unwrap()
    );
    // The zero-write contract is pinned at the store boundary, not by the
    // final row count: a stash that finish_schema_compression later rolled
    // back would also leave len() == 0 while still exercising the write
    // and its failure paths during dry-run.
    assert_eq!(
        concrete.writes.load(Ordering::Relaxed),
        0,
        "dry run must not write the store — not even rows it later rolls back"
    );
    assert_eq!(
        concrete.reads.load(Ordering::Relaxed),
        0,
        "dry run must not read the store either"
    );
    assert_eq!(concrete.len(), 0);
    assert!(
        outcome.stats.measured_output.len() < serde_json::to_string(&request.tools).unwrap().len(),
        "the measured candidate must still be reported"
    );
}

#[test]
fn before_model_applied_emitted_markers_authorize_retrieve_of_verbatim_originals() {
    // End-to-end: every emitted marker in response.tools authorizes a
    // retrieve of the verbatim original description, and the rows
    // survive because the rollback session was cleared on emit. One
    // marker is quoted back in UPPERCASE to pin case normalization.
    let concrete = Arc::new(ReadCountingStore::default());
    let store: Arc<dyn StashStore> = concrete.clone();
    let originals: Vec<String> = ["read", "write"]
        .iter()
        .map(|name| {
            serde_json::to_string(&long_tool(name))
                .unwrap()
                .parse::<Value>()
                .unwrap()["function"]["description"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    let request = BeforeModelRequest {
        tools: vec![long_tool("read"), long_tool("write")],
        visible_context: json!({"messages": []}),
        capabilities: BeforeModelCapabilities {
            replace_tools: true,
            recovery: RecoveryMethod::tool("tenant_retrieve").unwrap(),
        },
    };
    let outcome = before_model_with_store(&request, &options(), Some(&store)).unwrap();
    assert_eq!(outcome.stats.disposition, Disposition::Applied);
    assert_eq!(concrete.len(), 2, "both rows survive the clear-on-emit");
    assert_eq!(
        concrete.writes.load(Ordering::Relaxed),
        2,
        "exactly one stash write per truncated description, none deleted"
    );
    assert_eq!(outcome.artifact_keys.len(), 2);
    assert_eq!(outcome.stats.recoverability, Recoverability::Retrievable);

    // `artifact_keys` mirrors the compressor's pending-key HashMap, whose
    // iteration order is an implementation detail with no tie to the
    // read/write input order — zip-positioning `originals` against it
    // only passes when the hash seed happens to cooperate. The stash key
    // of each original is its BLAKE3, so pair every original with its
    // expected key, sort by key, and compare against the sorted emitted
    // keys: the expectation is derived from the keys themselves, and the
    // retrieve loop below stays key-aligned.
    let mut expected: Vec<(String, &String)> = originals
        .iter()
        .map(|original| (compute_key(original.as_bytes()), original))
        .collect();
    expected.sort_by(|a, b| a.0.cmp(&b.0));
    let mut emitted = outcome.artifact_keys.clone();
    emitted.sort();
    assert_eq!(
        emitted,
        expected
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>(),
        "artifact keys must be exactly the stash keys of the two originals"
    );

    for (index, (key, original)) in expected.iter().enumerate() {
        let quoted = if index == 0 {
            key.to_ascii_uppercase()
        } else {
            key.clone()
        };
        let restored = retrieve_authorized_with_store(
            &RetrieveRequest {
                hash_or_marker: quoted,
                visible_markers: outcome.response.visible_markers.clone(),
            },
            Some(&store),
            None,
            &Attribution::new("test"),
            "test",
        )
        .unwrap();
        assert_eq!(
            restored.payload,
            original.as_str(),
            "retrieve must return the verbatim original description"
        );
        assert!(
            !restored.payload.contains("tokenless retrieve"),
            "the payload is the original, not a retrieval instruction"
        );
    }
}

#[test]
fn before_model_obeys_replace_capability_without_owning_tool_names() {
    let tool = json!({
        "type": "function",
        "function": {
            "name": "tokenless_retrieve",
            "description": "description ".repeat(100),
            "parameters": {"type": "object"}
        }
    });
    let mut request = BeforeModelRequest {
        tools: vec![tool.clone()],
        visible_context: json!({}),
        capabilities: BeforeModelCapabilities {
            replace_tools: false,
            recovery: tokenless_protocol::RecoveryMethod::None,
        },
    };
    let outcome = before_model_with_store(&request, &options(), None).unwrap();
    assert_eq!(outcome.response.tools, vec![tool]);
    assert_eq!(outcome.stats.disposition, Disposition::Passthrough);

    request.capabilities.recovery = RecoveryMethod::tool("tokenless_retrieve").unwrap();
    let outcome = before_model_with_store(&request, &options(), None).unwrap();
    assert_eq!(outcome.response.tools, request.tools);
}

#[test]
fn path_list_routing_keeps_file_and_rtk_guards_and_records_stats() {
    let input: String = (0..60)
        .map(|i| format!("/workspace/project with spaces/long_directory/records/file {i}.json\n"))
        .collect();
    let mut request = post_tool_request(&input);
    request.tool_name = "Bash".into();
    request.content_origin = ContentOrigin::CommandOutput;
    let outcome = post_tool_with_store(&request, &options(), None).unwrap();
    assert_eq!(outcome.response.disposition, Disposition::Applied);
    assert_eq!(
        outcome.stats.content_type.as_deref(),
        Some("search_results")
    );
    assert_eq!(
        outcome.stats.applied_operations,
        [AppliedOperation::SearchPathSharing]
    );
    assert_eq!(outcome.response.recoverability, Recoverability::Lossless);
    assert!(outcome.response.stash_keys.is_empty());

    for (command, origin, optimization) in [
        (None, ContentOrigin::FileContent, OutputOptimization::None),
        (None, ContentOrigin::FileRead, OutputOptimization::None),
        (
            Some("cat paths.txt"),
            ContentOrigin::CommandOutput,
            OutputOptimization::None,
        ),
        (None, ContentOrigin::CommandOutput, OutputOptimization::Rtk),
    ] {
        request.command = command.map(str::to_owned);
        request.content_origin = origin;
        request.output_optimization = optimization;
        let outcome = post_tool_with_store(&request, &options(), None).unwrap();
        assert_eq!(outcome.response.output, input);
        assert!(outcome.response.applied_operations.is_empty());
    }
}

#[test]
fn search_path_sharing_records_stats_and_respects_rtk_ownership() {
    let content = "crates/long_directory/src/file.rs:12:matching source text\n".repeat(20);
    let mut request = post_tool_request(&content);
    request.tool_name = "Grep".into();
    request.content_origin = ContentOrigin::ApiResponse;
    let outcome = post_tool_with_store(&request, &options(), None).unwrap();
    assert_eq!(outcome.response.disposition, Disposition::Applied);
    assert_eq!(
        outcome.stats.applied_operations,
        [AppliedOperation::SearchPathSharing]
    );
    assert_eq!(outcome.response.recoverability, Recoverability::Lossless);
    assert!(outcome.response.stash_keys.is_empty());

    request.tool_name = "Bash".into();
    request.content_origin = ContentOrigin::CommandOutput;
    for optimization in [OutputOptimization::None, OutputOptimization::Rtk] {
        request.output_optimization = optimization;
        let outcome = post_tool_with_store(&request, &options(), None).unwrap();
        assert_eq!(outcome.response.disposition, Disposition::Passthrough);
        assert_eq!(outcome.response.output, content);
        assert!(outcome.stats.applied_operations.is_empty());
    }
}

#[test]
fn disabling_search_path_sharing_keeps_other_tools_compressible() {
    let mut options = options();
    options.search_path_sharing_enabled = false;
    for tool_name in ["Grep", "SearchFiles"] {
        let content = "crates/long_directory/src/file.rs:12:matching source text\n".repeat(20);
        let mut request = post_tool_request(&content);
        request.tool_name = tool_name.into();
        request.content_origin = ContentOrigin::ApiResponse;
        let outcome = post_tool_with_store(&request, &options, None).unwrap();
        assert_eq!(outcome.response.output, content);
        assert_eq!(outcome.response.disposition, Disposition::Passthrough);
        assert!(outcome.stats.applied_operations.is_empty());
        assert!(outcome.response.stash_keys.is_empty());
    }
    let input = format!(
        "{{\n{}\"debug\": null,\n{}\"value\": 1\n}}",
        " ".repeat(600),
        " ".repeat(600)
    );
    let mut request = post_tool_request(&input);
    request.tool_name = "SearchFiles".into();
    request.content_origin = ContentOrigin::ApiResponse;
    let outcome = post_tool_with_store(&request, &options, None).unwrap();
    assert_eq!(outcome.response.disposition, Disposition::Applied);
    assert!(!outcome.stats.applied_operations.is_empty());
}

#[test]
fn rtk_and_retrieve_results_bypass_post_tool_pipeline() {
    for (kind, optimization) in [
        (ResultKind::Tool, OutputOptimization::Rtk),
        (ResultKind::Retrieve, OutputOptimization::None),
    ] {
        let request = PostToolRequest {
            result_kind: kind,
            tool_name: "Bash".into(),
            content: r#"{"debug":"remove me","value":1}"#.into(),
            status: ToolResultStatus::Success,
            content_origin: ContentOrigin::CommandOutput,
            command: None,
            output_optimization: optimization,
            capabilities: PostToolCapabilities {
                replace_output: true,
                recovery: tokenless_protocol::RecoveryMethod::None,
                replace_with_text: true,
            },
            exit_code: None,
            stderr: None,
            original_command: None,
            cwd: None,
        };
        let outcome = post_tool_with_store(&request, &options(), None).unwrap();
        assert_eq!(outcome.response.disposition, Disposition::Passthrough);
        assert!(outcome.response.applied_operations.is_empty());
    }
}

#[test]
fn confirmed_unchanged_rtk_output_reenters_post_tool_once() {
    let directory = tempdir().unwrap();
    let mut options = options();
    options.rtk_data_dir = Some(directory.path().to_path_buf());
    let attribution = Attribution {
        agent_id: "test".into(),
        session_id: Some("session".into()),
        tool_use_id: Some("call".into()),
    };
    let content = serde_json::to_string_pretty(
        &(0..30)
            .map(|index| serde_json::json!({"name": format!("item-{index}"), "value": index}))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let mut request = post_tool_request(&content);
    request.output_optimization = OutputOptimization::Rtk;
    let mut store = OutcomeStore::open(directory.path(), true).unwrap();
    for changed in [true, false] {
        let run = store
            .begin("test", "session", "call", 1, "git log", str::to_owned)
            .unwrap()
            .unwrap();
        store.record(&run, 0, changed).unwrap();
        request.command = Some(run);
        let outcome = post_tool_with_outcomes(&request, &options, None, &attribution).unwrap();
        assert_eq!(
            outcome.response.disposition,
            if changed {
                Disposition::Passthrough
            } else {
                Disposition::Applied
            }
        );
        if !changed {
            assert_eq!(outcome.response.recoverability, Recoverability::Lossless);
            assert!(outcome.response.after_tokens < outcome.response.before_tokens);
        }
        let repeated = post_tool_with_outcomes(&request, &options, None, &attribution).unwrap();
        assert_eq!(repeated.response.output, request.content);
        assert_eq!(repeated.response.disposition, Disposition::Passthrough);
    }
}

#[test]
fn rtk_fallback_preserves_error_file_and_retrieve_guards() {
    let directory = tempdir().unwrap();
    let mut options = options();
    options.rtk_data_dir = Some(directory.path().to_path_buf());
    let attribution = Attribution {
        agent_id: "test".into(),
        session_id: Some("session".into()),
        tool_use_id: Some("call".into()),
    };
    let content = serde_json::to_string_pretty(
        &(0..30)
            .map(|index| serde_json::json!({"name": format!("item-{index}"), "value": index}))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    for (status, origin, kind) in [
        (
            ToolResultStatus::Error,
            ContentOrigin::CommandOutput,
            ResultKind::Tool,
        ),
        (
            ToolResultStatus::Success,
            ContentOrigin::FileContent,
            ResultKind::Tool,
        ),
        (
            ToolResultStatus::Success,
            ContentOrigin::CommandOutput,
            ResultKind::Retrieve,
        ),
        (
            ToolResultStatus::Denied,
            ContentOrigin::CommandOutput,
            ResultKind::Tool,
        ),
    ] {
        let mut store = OutcomeStore::open(directory.path(), true).unwrap();
        let run = store
            .begin("test", "session", "call", 1, "git log", str::to_owned)
            .unwrap()
            .unwrap();
        store.record(&run, 0, false).unwrap();
        let request = PostToolRequest {
            status,
            content_origin: origin,
            result_kind: kind,
            output_optimization: OutputOptimization::Rtk,
            command: Some(run),
            ..post_tool_request(&content)
        };
        let outcome = post_tool_with_outcomes(&request, &options, None, &attribution).unwrap();
        assert_eq!(outcome.response.output, content);
        assert!(outcome.response.applied_operations.is_empty());
    }
}

#[test]
fn post_tool_reports_plain_file_prints_as_file_read() {
    let content = r#"{"debug":"remove me","value":1}"#;
    for (origin, command, expected) in [
        (ContentOrigin::CommandOutput, "cat data.json", "file_read"),
        (
            ContentOrigin::CommandOutput,
            "cat data.json | jq .",
            "command_output",
        ),
        (ContentOrigin::ApiResponse, "cat data.json", "api_response"),
        // Quiet spellings beyond `-n` are plain prints too.
        (
            ContentOrigin::CommandOutput,
            "sed --quiet '1,80p' data.json",
            "file_read",
        ),
        (
            ContentOrigin::CommandOutput,
            "sed -ne '5p' data.json",
            "file_read",
        ),
        // A substituting script rides along after a print-only one.
        (
            ContentOrigin::CommandOutput,
            "sed -n -e 5p -e s/./x/p data.json",
            "command_output",
        ),
    ] {
        let request = PostToolRequest {
            content_origin: origin,
            command: Some(command.into()),
            ..post_tool_request(content)
        };
        let outcome = post_tool_with_store(&request, &options(), None).unwrap();
        assert_eq!(
            outcome.stats.content_origin.as_deref(),
            Some(expected),
            "{command}"
        );
    }
}

#[test]
fn post_tool_preserves_printed_html_and_reports_file_read_statistics() {
    let content = format!(
        "<html><body><main>{}</main></body></html>",
        "source text ".repeat(200)
    );
    let request = PostToolRequest {
        content_origin: ContentOrigin::CommandOutput,
        command: Some("cat page.html".into()),
        ..post_tool_request(&content)
    };
    let mut opts = options();
    opts.html_extraction_enabled = true;
    let outcome = post_tool_with_store(&request, &opts, None).unwrap();
    assert_eq!(outcome.stats.content_origin.as_deref(), Some("file_read"));
    assert_eq!(outcome.response.output, content);
    assert!(outcome.response.applied_operations.is_empty());
}

#[test]
fn post_tool_routes_statuses_before_the_json_pipeline() {
    for status in [ToolResultStatus::Interrupted, ToolResultStatus::Denied] {
        let request = post_tool_request(r#"{"debug":"remove me","value":1}"#);
        let request = PostToolRequest { status, ..request };
        let outcome = post_tool_with_store(&request, &options(), None).unwrap();
        assert_eq!(outcome.response.disposition, Disposition::Passthrough);
        assert_eq!(outcome.response.output, request.content);
    }

    let request = PostToolRequest {
        status: ToolResultStatus::Error,
        content: "/bin/sh: jq: command not found".into(),
        ..post_tool_request("unused")
    };
    let outcome = post_tool_with_store(&request, &options(), None).unwrap();
    assert_eq!(outcome.response.disposition, Disposition::ToolError);
    assert_eq!(outcome.response.output, request.content);
    assert!(outcome.response.additional_context.is_none());
}

fn failing_build_log() -> String {
    let mut output = "$ cargo build\n".to_owned();
    for index in 0..30 {
        output.push_str(&format!(
            "Compiling package-{index:03} v0.1.{index} with extended progress output\n"
        ));
    }
    output.push_str("error: linker command not found\n");
    output
}

#[test]
fn tool_error_build_log_compresses_when_exit_code_is_present_without_env_diagnosis() {
    let store: Arc<dyn StashStore> = Arc::new(InMemoryStore::new());
    let request = PostToolRequest {
        status: ToolResultStatus::Error,
        content: failing_build_log(),
        exit_code: Some(101),
        capabilities: PostToolCapabilities {
            replace_output: true,
            recovery: tokenless_protocol::RecoveryMethod::Shell,
            replace_with_text: true,
        },
        ..post_tool_request("unused")
    };

    let outcome = post_tool_with_store(&request, &options(), Some(&store)).unwrap();

    assert_eq!(outcome.response.disposition, Disposition::Applied);
    assert_eq!(outcome.response.content_type, Some(ContentType::BuildLog));
    assert_eq!(
        outcome.response.applied_operations,
        [AppliedOperation::BuildLogReduction]
    );
    assert_eq!(outcome.response.recoverability, Recoverability::Retrievable);
    assert!(!outcome.response.stash_keys.is_empty());
    assert!(
        outcome
            .response
            .output
            .contains("error: linker command not found")
    );
    assert!(!outcome.response.output.contains("Compiling package-015"));
    assert!(outcome.response.additional_context.is_none());
}

#[test]
fn tool_error_without_exit_code_keeps_original_build_log_without_env_diagnosis() {
    let store: Arc<dyn StashStore> = Arc::new(InMemoryStore::new());
    for recovery in [
        tokenless_protocol::RecoveryMethod::None,
        tokenless_protocol::RecoveryMethod::Shell,
        tokenless_protocol::RecoveryMethod::tool("tokenless_retrieve").unwrap(),
    ] {
        let request = PostToolRequest {
            status: ToolResultStatus::Error,
            content: failing_build_log(),
            capabilities: PostToolCapabilities {
                replace_output: true,
                recovery,
                replace_with_text: true,
            },
            ..post_tool_request("unused")
        };
        let outcome = post_tool_with_store(&request, &options(), Some(&store)).unwrap();
        assert_eq!(outcome.response.disposition, Disposition::ToolError);
        assert_eq!(outcome.response.output, request.content);
        assert!(outcome.response.applied_operations.is_empty());
        assert!(outcome.response.stash_keys.is_empty());
        assert!(outcome.response.additional_context.is_none());
    }
}

#[test]
fn post_tool_accepts_lossless_json_and_requires_retrieve_for_truncation() {
    let cleanup = post_tool_request(&format!(
        "{{\n{}\"debug\": null,\n{}\"value\": \"kept\"\n}}",
        " ".repeat(600),
        " ".repeat(600)
    ));
    let outcome = post_tool_with_store(&cleanup, &options(), None).unwrap();
    assert_eq!(outcome.response.disposition, Disposition::Applied);
    assert_eq!(outcome.response.recoverability, Recoverability::Lossless);
    assert_eq!(
        outcome.response.applied_operations,
        vec![AppliedOperation::JsonCleanup]
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&outcome.response.output).unwrap(),
        serde_json::from_str::<serde_json::Value>(&cleanup.content).unwrap()
    );

    let lossy = post_tool_request(&serde_json::to_string(&(0..300).collect::<Vec<_>>()).unwrap());
    let unavailable = Arc::new(ReadCountingStore::default());
    let unavailable_store: Arc<dyn StashStore> = unavailable.clone();
    let rejected = post_tool_with_store(&lossy, &options(), Some(&unavailable_store)).unwrap();
    assert_eq!(
        rejected.response.disposition,
        Disposition::RecoverabilityUnavailable
    );
    assert_eq!(rejected.response.output, lossy.content);
    // Same zero-write discipline as the dry-run path: an empty final
    // store would not distinguish "never attached" from "stashed and
    // rolled back", so pin the stash-call counter.
    assert_eq!(unavailable.writes.load(Ordering::Relaxed), 0);
    assert!(unavailable.is_empty());

    let failing: Arc<dyn StashStore> = Arc::new(FailingStore);
    let failing_request = PostToolRequest {
        capabilities: PostToolCapabilities {
            recovery: tokenless_protocol::RecoveryMethod::Shell,
            ..lossy.capabilities
        },
        ..lossy.clone()
    };
    assert!(matches!(
        post_tool_with_store(&failing_request, &options(), Some(&failing)),
        Err(RuntimeError::StashWrite { count }) if count > 0
    ));

    let store: Arc<dyn StashStore> = Arc::new(InMemoryStore::new());
    let recoverable = PostToolRequest {
        capabilities: PostToolCapabilities {
            recovery: tokenless_protocol::RecoveryMethod::Shell,
            ..lossy.capabilities
        },
        ..lossy
    };
    let applied = post_tool_with_store(&recoverable, &options(), Some(&store)).unwrap();
    assert_eq!(applied.response.disposition, Disposition::Applied);
    assert_eq!(applied.response.recoverability, Recoverability::Retrievable);
    assert!(!applied.response.stash_keys.is_empty());
}

#[test]
fn post_tool_preserves_json_values_in_text_and_structured_slots() {
    let original = json!({
        "debug": "x".repeat(1000), "trace": null, "traces": [],
        "stack": {}, "stacktrace": "", "logs": [null, {}, "", [], false, 0],
        "logging": {"debug": null, "items": [1, null, 2]},
        "empty": null
    });
    for content in [
        original.to_string(),
        serde_json::to_string_pretty(&original).unwrap(),
    ] {
        for replace_with_text in [false, true] {
            for recovery in [
                tokenless_protocol::RecoveryMethod::None,
                tokenless_protocol::RecoveryMethod::Shell,
            ] {
                for have_store in [false, true] {
                    let concrete = Arc::new(ReadCountingStore::default());
                    let store: Arc<dyn StashStore> = concrete.clone();
                    let mut request = post_tool_request(&content);
                    request.content_origin = ContentOrigin::ApiResponse;
                    request.capabilities.replace_with_text = replace_with_text;
                    request.capabilities.recovery = recovery.clone();
                    let outcome =
                        post_tool_with_store(&request, &options(), have_store.then_some(&store))
                            .unwrap();
                    let decoded: serde_json::Value = if outcome
                        .response
                        .applied_operations
                        .contains(&AppliedOperation::Toon)
                    {
                        toon_format::decode_default(&outcome.response.output).unwrap()
                    } else {
                        serde_json::from_str(&outcome.response.output).unwrap()
                    };
                    assert_eq!(decoded, original);
                    assert_eq!(outcome.response.recoverability, Recoverability::Lossless);
                    assert!(outcome.response.stash_keys.is_empty());
                    assert_eq!(concrete.writes.load(Ordering::Relaxed), 0);
                }
            }
        }
    }
}

#[test]
fn post_tool_applies_lossless_json_envelope_unwrapping() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../tests/contract/fixtures/post_tool/double_encoded.json"
    ))
    .unwrap();
    let content = fixture["tool_response"].as_str().unwrap();
    let inner: String = serde_json::from_str(content).unwrap();
    let original: Value = serde_json::from_str(&inner).unwrap();
    for replace_with_text in [false, true] {
        let concrete = Arc::new(ReadCountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let mut request = post_tool_request(content);
        request.content_origin = ContentOrigin::ApiResponse;
        request.capabilities.replace_with_text = replace_with_text;
        let outcome = post_tool_with_store(&request, &options(), Some(&store)).unwrap();
        assert_eq!(outcome.response.disposition, Disposition::Applied);
        assert_eq!(
            outcome.response.applied_operations,
            [AppliedOperation::JsonCleanup]
        );
        assert_eq!(outcome.response.recoverability, Recoverability::Lossless);
        assert_eq!(
            serde_json::from_str::<Value>(&outcome.response.output).unwrap(),
            original
        );
        assert!(outcome.response.stash_keys.is_empty());
        assert_eq!(concrete.writes.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn diagnostic_field_bounds_require_recovery_and_preserve_empty_siblings() {
    let original =
        json!({"debug": "x".repeat(100_000), "empty": null, "items": [null, {}, [], ""]});
    for reachable in [false, true] {
        let store: Arc<dyn StashStore> = Arc::new(InMemoryStore::new());
        let mut request = post_tool_request(&original.to_string());
        request.capabilities.replace_with_text = false;
        if reachable {
            request.capabilities.recovery = tokenless_protocol::RecoveryMethod::Shell;
        }
        let outcome = post_tool_with_store(&request, &options(), Some(&store)).unwrap();
        if reachable {
            assert_eq!(outcome.response.disposition, Disposition::Applied);
            assert_eq!(outcome.response.recoverability, Recoverability::Retrievable);
            assert_eq!(
                outcome.response.applied_operations,
                [AppliedOperation::JsonTruncation]
            );
            assert_eq!(outcome.response.stash_keys.len(), 1);
            let recovered = store
                .retrieve(&outcome.response.stash_keys[0])
                .unwrap()
                .unwrap();
            assert_eq!(recovered, original["debug"].as_str().unwrap());
            let mut output: serde_json::Value =
                serde_json::from_str(&outcome.response.output).unwrap();
            output["debug"] = json!(recovered);
            assert_eq!(output, original);
        } else {
            assert_eq!(
                outcome.response.disposition,
                Disposition::RecoverabilityUnavailable
            );
            assert_eq!(outcome.response.output, request.content);
            assert!(outcome.response.stash_keys.is_empty());
        }
    }
}

#[test]
fn post_tool_retains_numbers_the_parser_cannot_represent() {
    for literal in [
        "18446744073709551617",
        "-9223372036854775809",
        "1.234567890123456789",
        "1e-400",
    ] {
        let content = format!(
            "{{\n{}\"value\": {literal},\n{}\"label\": \"synthetic\"\n}}",
            " ".repeat(600),
            " ".repeat(600)
        );
        for replace_with_text in [false, true] {
            let mut request = post_tool_request(&content);
            request.content_origin = ContentOrigin::ApiResponse;
            request.capabilities.replace_with_text = replace_with_text;
            let outcome = post_tool_with_store(&request, &options(), None).unwrap();
            // One unrepresentable number no longer costs the whole
            // response its compression: the literal is retained and the
            // rest of the document is compacted around it.
            assert_eq!(outcome.response.disposition, Disposition::Applied);
            assert_eq!(outcome.response.recoverability, Recoverability::Lossless);
            assert_eq!(
                outcome.response.applied_operations,
                [AppliedOperation::JsonCleanup]
            );
            assert!(outcome.response.stash_keys.is_empty());
            assert!(
                outcome
                    .response
                    .output
                    .contains(&format!("\"value\":{literal},")),
                "{literal} missing from {}",
                outcome.response.output
            );
            assert!(outcome.response.output.len() < content.len());
        }
    }
}

#[test]
fn html_stash_write_failure_reports_stashwrite_not_pipeline() {
    // When the stash backend rejects the pre-candidate write, the HTML
    // domain degrades to the unchanged candidate and counts the failed
    // write. The lifecycle must then classify the failure as a stash
    // write (the taxonomy every other domain already uses), never as
    // the generic Pipeline compressor error that used to fail the call.
    let page = format!(
        "<!DOCTYPE html><html><head><title>Doc</title><style>{}</style></head>\
         <body><main><h1>Title</h1>{}</main></body></html>",
        "body{margin:0;padding:0}".repeat(64),
        (0..10)
            .map(|i| format!("<p>Paragraph {i}: {}</p>", "readable words ".repeat(12)))
            .collect::<String>()
    );
    let request = PostToolRequest {
        capabilities: PostToolCapabilities {
            recovery: tokenless_protocol::RecoveryMethod::Shell,
            ..post_tool_request(&page).capabilities
        },
        ..post_tool_request(&page)
    };
    let mut opts = options();
    opts.html_extraction_enabled = true;
    let failing: Arc<dyn StashStore> = Arc::new(FailingStore);
    assert!(matches!(
        post_tool_with_store(&request, &opts, Some(&failing)),
        Err(RuntimeError::StashWrite { count }) if count == 1
    ));
}

#[test]
fn pnpm_entry_preserves_host_guards_and_rtk_ownership() {
    let input = include_str!(
        "../../../tokenless-compressors/tests/fixtures/build_logs/pnpm_install_live.txt"
    );
    for mode in 0..8 {
        let mut request = post_tool_request(input);
        match mode {
            0 => request.status = ToolResultStatus::Error,
            1 => request.status = ToolResultStatus::Denied,
            2 => request.status = ToolResultStatus::Interrupted,
            3 => request.result_kind = ResultKind::Retrieve,
            4 => request.output_optimization = OutputOptimization::Rtk,
            5 => request.content_origin = ContentOrigin::FileContent,
            6 => request.tool_name = "Grep".into(),
            7 => request.capabilities.replace_output = false,
            _ => unreachable!(),
        }
        let result = post_tool_with_store(&request, &options(), None).unwrap();
        assert_eq!(result.response.output, input, "mode {mode}");
        assert!(result.response.applied_operations.is_empty());
        assert!(result.response.stash_keys.is_empty());
    }
    assert!(!package_command_is_build_log_owned(&["install".into()]));
}

#[test]
fn pnpm_entry_reports_storage_failure_even_with_profitable_layout() {
    let input = include_str!(
        "../../../tokenless-compressors/tests/fixtures/build_logs/pnpm_install_live.txt"
    );
    let mut request = post_tool_request(input);
    request.capabilities.recovery = tokenless_protocol::RecoveryMethod::Shell;
    let failing: Arc<dyn StashStore> = Arc::new(FailingStore);
    assert!(matches!(
        post_tool_with_store(&request, &options(), Some(&failing)),
        Err(RuntimeError::StashWrite { count: 1 })
    ));
}

fn post_tool_request(content: &str) -> PostToolRequest {
    PostToolRequest {
        result_kind: ResultKind::Tool,
        tool_name: "Bash".into(),
        content: content.into(),
        status: ToolResultStatus::Success,
        content_origin: ContentOrigin::CommandOutput,
        command: None,
        output_optimization: OutputOptimization::None,
        capabilities: PostToolCapabilities {
            replace_output: true,
            recovery: tokenless_protocol::RecoveryMethod::None,
            replace_with_text: true,
        },
        exit_code: None,
        stderr: None,
        original_command: None,
        cwd: None,
    }
}

#[test]
fn pre_tool_replacement_none_disables_search_and_build_exemptions() {
    for command in [
        "cargo test --workspace",
        "timeout 30 cargo build",
        "nice -n 10 pytest -q",
        "grep -n needle src",
        "find . -type f",
    ] {
        let mut request = bash_request(command);
        request.capabilities.post_tool_replacement = PostToolReplacementCapability::None;
        let response = pre_tool_with_rewriter(
            &request,
            &Attribution::new("test"),
            Some(Path::new("/opt/tokenless/tokenless")),
            Some(Path::new("/tmp/tokenless-test")),
            |cmd| rtk::RewriteOutcome::Allow(format!("rtk {cmd}")),
        )
        .unwrap();
        assert_eq!(
            response.action,
            PreToolAction::ReplaceArguments,
            "{command}"
        );
        assert_eq!(response.output_optimization, OutputOptimization::Rtk);
    }
}

#[test]
fn pre_tool_replacement_lossless_only_keeps_search_native_and_rewrites_build_and_vcs() {
    for (command, expect_rtk) in [
        ("grep -rn needle src", false),
        ("find . -type f", false),
        ("head -40 README.md", false),
        ("cargo test --workspace", true),
        ("git status", true),
    ] {
        let mut request = bash_request(command);
        request.capabilities.post_tool_replacement = PostToolReplacementCapability::LosslessOnly;
        let response = pre_tool_with_rewriter(
            &request,
            &Attribution::new("test"),
            Some(Path::new("/opt/tokenless/tokenless")),
            Some(Path::new("/tmp/tokenless-test")),
            |cmd| rtk::RewriteOutcome::Allow(format!("rtk {cmd}")),
        )
        .unwrap();
        assert_eq!(
            response.action == PreToolAction::ReplaceArguments,
            expect_rtk,
            "{command}"
        );
    }
}

#[test]
fn pre_tool_unified_wrapper_stripping_keeps_wrapped_build_test_native() {
    for command in [
        "timeout 30 cargo test --workspace",
        "uv run pytest -q",
        "uv run python3 -m pytest tests/",
        "nice -n 5 make -j4",
        "env -S 'timeout 30 cargo test --workspace'",
        "env -vS 'cargo test --workspace'",
        "env -S 'cargo\\_test\\_--workspace'",
    ] {
        let request = bash_request(command);
        let response = pre_tool_with_rewriter(
            &request,
            &Attribution::new("test"),
            Some(Path::new("/opt/tokenless/tokenless")),
            Some(Path::new("/tmp/tokenless-test")),
            rtk_must_not_run,
        )
        .unwrap();
        assert_eq!(response.action, PreToolAction::Passthrough, "{command}");
    }
}

#[test]
fn post_tool_tokenless_retrieve_shell_command_bypasses_and_skips_env_diagnosis() {
    let payload = serde_json::to_string_pretty(
        &(0..30)
            .map(|i| serde_json::json!({"index": i, "payload": "x".repeat(40)}))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    for command in [
        "/usr/local/bin/tokenless --data-dir /tmp/d retrieve aabbccdd1122",
        "env -C /tmp/work tokenless retrieve aabbccdd1122",
        "env --chdir /tmp/work tokenless retrieve aabbccdd1122",
        "env -C /tmp/work -- tokenless retrieve aabbccdd1122",
        "env -S 'tokenless retrieve aabbccdd1122'",
        "env -S 'tokenless\\_retrieve\\_aabbccdd1122'",
        "env -vS 'tokenless retrieve aabbccdd1122'",
        "env -iS'tokenless retrieve aabbccdd1122'",
        "env -vC /tmp/work tokenless retrieve aabbccdd1122",
        "env -S 'tokenless retrieve aabbccdd1122\\cignored'",
        "env -S 'tokenless --data-dir \"/tmp/my\\_dir\" retrieve aabbccdd1122'",
        "env --split-string='FOO=1 -C /tmp/work tokenless retrieve aabbccdd1122'",
        "env -S 'tokenless --data-dir /tmp/d retrieve' aabbccdd1122",
    ] {
        let ok_req = PostToolRequest {
            result_kind: ResultKind::Tool,
            command: Some(command.into()),
            capabilities: PostToolCapabilities {
                replace_output: true,
                recovery: tokenless_protocol::RecoveryMethod::Shell,
                replace_with_text: true,
            },
            ..post_tool_request(&payload)
        };
        let ok_outcome = post_tool_with_store(&ok_req, &options(), None).unwrap();
        assert_eq!(
            ok_outcome.response.disposition,
            Disposition::Passthrough,
            "{command}"
        );
        assert_eq!(ok_outcome.response.output, payload, "{command}");
    }

    assert!(!super::is_tokenless_retrieve_command(
        "env -S '\ntokenless retrieve aabbccdd1122'"
    ));
    assert!(!super::is_tokenless_retrieve_command(
        "env -S 'tokenless\nretrieve aabbccdd1122'"
    ));

    let err_req = PostToolRequest {
        result_kind: ResultKind::Tool,
        status: ToolResultStatus::Error,
        command: Some("tokenless retrieve aabbccdd1122".into()),
        stderr: Some("error: permission denied opening stash.db".into()),
        exit_code: Some(1),
        ..post_tool_request("failed to retrieve")
    };
    let err_outcome = post_tool_with_store(&err_req, &options(), None).unwrap();
    assert_eq!(err_outcome.response.disposition, Disposition::ToolError);
    assert_eq!(err_outcome.response.output, "failed to retrieve");
    assert!(err_outcome.response.additional_context.is_none());
}
