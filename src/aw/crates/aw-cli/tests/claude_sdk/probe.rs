use crate::support::{Fixture, Process};
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt, time::Duration};

#[test]
fn slow_python_startup_allows_both_claude_probes() {
    let fixture = Fixture::new();
    let python_path = fixture.root.join("pythonpath");
    let sdk = python_path.join("claude_agent_sdk");
    fs::create_dir_all(sdk.join("_bundled")).unwrap();
    fs::create_dir_all(sdk.join("_internal/transport")).unwrap();
    for source in [
        "__init__.py",
        "_internal/query.py",
        "_internal/transport/subprocess_cli.py",
        "types.py",
    ] {
        fs::write(sdk.join(source), "").unwrap();
    }
    let cli = sdk.join("_bundled/claude");
    fs::write(&cli, "#!/bin/sh\necho '2.1.161 (Claude Code)'\n").unwrap();
    fs::set_permissions(cli, fs::Permissions::from_mode(0o700)).unwrap();
    let dist_info = python_path.join("claude_agent_sdk-0.2.88.dist-info");
    fs::create_dir(&dist_info).unwrap();
    fs::write(
        dist_info.join("METADATA"),
        "Name: claude-agent-sdk\nVersion: 0.2.88\n",
    )
    .unwrap();
    fs::write(
        python_path.join("sitecustomize.py"),
        "import time\n\ntime.sleep(6)\nwith open('probe-startups', 'a') as startup:\n    startup.write('ready\\n')\n",
    )
    .unwrap();

    let mut document = fixture.document();
    document["spec"]["agents"] =
        json!({"sdk": {"adapter": "claude-sdk", "argv": [fixture.python, "worker.py"]}});
    document["spec"]["events"]["security.violation"] = json!({"enabled": true, "steps": []});
    fixture.save(&document);
    let mut command = fixture.command();
    command
        .args(["run", "--config", "aw.json", "--agent", "sdk"])
        .env("PYTHONPATH", python_path);
    let output = Process::spawn(&fixture, command).finish_with_timeout(Duration::from_secs(25));
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The fixture reports pinned release numbers, but has different source and CLI hashes.
    assert!(!output.status.success());
    assert!(
        stderr.contains("Adapter evidence lacks an enabled event"),
        "{stderr}"
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("probe-startups")).unwrap(),
        "ready\nready\n"
    );
}
