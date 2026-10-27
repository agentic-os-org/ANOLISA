//! Optional PowerShell execution checks on a Unix validation host.

use super::*;
use std::os::unix::fs::PermissionsExt;

#[test]
#[ignore = "requires TOKENLESS_TEST_POWERSHELL pointing to PowerShell 7"]
fn powershell_forwards_native_flags_pipelines_status_and_restores_scope() {
    let shell = std::env::var("TOKENLESS_TEST_POWERSHELL").unwrap();
    let root = tempfile::tempdir().unwrap();
    let executable = root.path().join("tokenless path");
    std::fs::write(
        &executable,
        "#!/bin/sh\n[ \"$1\" = rtk ] || exit 98\nshift\nexec /usr/bin/python3 \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let attribution = Attribution::new("test-agent");
    for (command, expected, status, exit) in [
        (
            "tokenless rtk -c 'import sys;print(sys.argv[1:]);sys.exit(7)' 'a b' -- -ErrorAction Stop -Verbose",
            "['a b', '--', '-ErrorAction', 'Stop', '-Verbose']",
            "False",
            7,
        ),
        (
            "tokenless rtk -c 'import sys;sys.exit(7)'; Write-Output done",
            "done",
            "True",
            7,
        ),
        (
            "'first','second' | tokenless rtk -c 'import sys;print(sys.stdin.read().strip())'",
            "first\nsecond",
            "True",
            0,
        ),
    ] {
        let script = anchor_powershell_rtk("", command, &executable, &attribution, root.path());
        let output = Command::new(&shell).args(["-NoProfile", "-NonInteractive", "-Command", &format!(
            "$env:TOKENLESS_AGENT_ID='before'; Set-Alias tokenless Write-Output; $ErrorActionPreference='Stop'; $Error.Clear(); {script}; [Console]::Write(\"ok=$?;exit=$LASTEXITCODE;env=$env:TOKENLESS_AGENT_ID;alias=$((Get-Alias tokenless).Definition);errors=$($Error.Count);preference=$ErrorActionPreference\")"
        )]).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let stdout = String::from_utf8(output.stdout)
            .unwrap()
            .replace("\r\n", "\n");
        assert!(stdout.contains(expected), "{stdout}");
        assert!(
            stdout.contains(&format!(
                "ok={status};exit={exit};env=before;alias=Write-Output;errors=0;preference=Stop"
            )),
            "{stdout}"
        );
    }
}

#[test]
#[ignore = "requires TOKENLESS_TEST_POWERSHELL pointing to PowerShell 7"]
fn powershell_receipts_track_each_wrapper_and_restore_inherited_receipt_context() {
    let shell = std::env::var("TOKENLESS_TEST_POWERSHELL").unwrap();
    let root = tempfile::tempdir().unwrap();
    let executable = root.path().join("tokenless path");
    std::fs::write(
        &executable,
        r#"#!/usr/bin/python3
import os, sqlite3
with sqlite3.connect(os.path.join(os.environ['TOKENLESS_DATA_DIR'], 'rtk-outcomes.db')) as db:
    db.execute('INSERT INTO outcomes (run, ordinal, changed) VALUES (?, ?, 0)',
               (os.environ['TOKENLESS_RTK_RUN_ID'], int(os.environ['TOKENLESS_RTK_ORDINAL'])))
print('unchanged')
"#,
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let attribution = Attribution {
        agent_id: "test-agent".into(),
        session_id: Some("session".into()),
        tool_use_id: Some("tool".into()),
    };
    let original = "git status; git log";
    let command = anchor_powershell_rtk(
        original,
        "tokenless rtk git status; tokenless rtk git log",
        &executable,
        &attribution,
        root.path(),
    );
    let output = Command::new(&shell).args(["-NoProfile", "-NonInteractive", "-Command", &format!(
        "$env:TOKENLESS_RTK_RUN_ID='inherited'; $env:TOKENLESS_RTK_ORDINAL='9'; {command}; [Console]::Write(\"run=$env:TOKENLESS_RTK_RUN_ID;ordinal=$env:TOKENLESS_RTK_ORDINAL\")"
    )]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("run=inherited;ordinal=9")
    );
    let mut store = OutcomeStore::open(root.path(), false).unwrap();
    assert_eq!(
        store
            .consume_unchanged("test-agent", "session", "tool", &command)
            .unwrap()
            .as_deref(),
        Some(original)
    );
    assert!(
        store
            .consume_unchanged("test-agent", "session", "tool", &command)
            .unwrap()
            .is_none()
    );
}
