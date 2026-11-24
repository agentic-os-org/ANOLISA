use crate::support::{Fixture, Process, Service};
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

fn install_sdk(python_path: &Path) {
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
}

#[test]
fn slow_python_startup_allows_both_claude_probes() {
    let fixture = Fixture::new();
    let python_path = fixture.root.join("pythonpath");
    install_sdk(&python_path);
    fs::write(
        python_path.join("sitecustomize.py"),
        "import time\nprint('Python startup banner with spaces', flush=True)\ntime.sleep(6)\nwith open('probe-startups', 'a') as startup:\n    startup.write('ready\\n')\n",
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

#[test]
fn standard_instrumentation_is_skipped_only_in_the_probe() {
    let fixture = Fixture::new();
    let auto = fixture
        .root
        .join("opt/opentelemetry/instrumentation/auto_instrumentation");
    let custom = fixture.root.join("custom");
    fs::create_dir_all(&auto).unwrap();
    install_sdk(&custom);
    fs::write(
        auto.join("sitecustomize.py"),
        r#"import os, sys
print("starting application telemetry", flush=True)
if sys.argv[0] == '-c':
    os._exit(89)
"#,
    )
    .unwrap();
    fs::write(
        custom.join("probe_helper.py"),
        "import os\nassert os.environ['OTEL_SDK_DISABLED'] == 'true'\n",
    )
    .unwrap();
    fs::write(
        custom.join("claude_agent_sdk/__init__.py"),
        "import probe_helper\n",
    )
    .unwrap();
    fs::write(
        fixture.root.join("worker.py"),
        r#"import json, os, sys
from pathlib import Path
Path('worker-env').write_text(json.dumps([os.environ['PYTHONPATH'].split(os.pathsep),
                                          os.environ.get('OTEL_SDK_DISABLED')]))
sys.exit(23)
"#,
    )
    .unwrap();
    let mut document = fixture.document();
    document["spec"]["agents"] =
        json!({"sdk": {"adapter": "claude-sdk", "argv": [fixture.python, "worker.py"]}});
    let service = Service::start(&fixture, &document);
    let paths = std::env::join_paths([&auto, &custom]).unwrap();
    let mut prepare = fixture.command();
    prepare
        .args([
            "instance", "prepare", "--config", "aw.json", "--agent", "sdk",
        ])
        .env("PYTHONPATH", &paths);
    let prepared = fixture.run(prepare);
    assert!(
        prepared.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let registration: serde_json::Value = serde_json::from_slice(&prepared.stdout).unwrap();
    assert_eq!(registration["state"], "pending_ready");
    let mut release = fixture.command();
    release.args([
        "instance",
        "release",
        "--binding",
        registration["binding"].as_str().unwrap(),
    ]);
    let released = fixture.run(release);
    assert!(
        released.status.success(),
        "{}",
        String::from_utf8_lossy(&released.stderr)
    );

    let mut run = fixture.command();
    run.args(["run", "--config", "aw.json", "--agent", "sdk"])
        .env("PYTHONPATH", paths);
    let output = fixture.run(run);
    assert_eq!(
        output.status.code(),
        Some(23),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("starting application telemetry"));
    let (worker_paths, disabled): (Vec<std::path::PathBuf>, Option<String>) =
        serde_json::from_slice(&fs::read(fixture.root.join("worker-env")).unwrap()).unwrap();
    assert_eq!(worker_paths[1..], [auto, custom]);
    assert_eq!(disabled, None);
    service.released(&fixture);
}

#[test]
fn stdout_cannot_supply_missing_or_invalid_version_evidence() {
    let fixture = Fixture::new();
    let custom = fixture.root.join("custom");
    install_sdk(&custom);
    fs::write(
        custom.join("sitecustomize.py"),
        "print('0.2.88:2.1.161 (Claude Code)', flush=True)\n",
    )
    .unwrap();
    let mut document = fixture.document();
    document["spec"]["agents"] =
        json!({"sdk": {"adapter": "claude-sdk", "argv": [fixture.python, "worker.py"]}});
    fixture.save(&document);
    let check = |expected: &str| {
        let mut command = fixture.command();
        command
            .args(["run", "--config", "aw.json", "--agent", "sdk"])
            .env("PYTHONPATH", &custom);
        let output = fixture.run(command);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success() && stderr.contains(expected),
            "{stderr}"
        );
        assert!(!fixture.root.join("state").exists());
    };
    let metadata = custom.join("claude_agent_sdk-0.2.88.dist-info/METADATA");
    fs::write(
        &metadata,
        "Name: claude-agent-sdk\nVersion: invalid version\n",
    )
    .unwrap();
    check("Claude SDK must report a version");
    fs::write(metadata, "Name: claude-agent-sdk\nVersion: 0.2.88\n").unwrap();
    let cli = custom.join("claude_agent_sdk/_bundled/claude");
    fs::write(&cli, "#!/bin/sh\necho '2.1.161'\n").unwrap();
    check("must identify its bundled Claude Code CLI");
    fs::remove_file(&cli).unwrap();
    check("probe exited unsuccessfully");
    fs::write(&cli, "#!/bin/sh\necho '2.1.161 (Claude Code)'\n").unwrap();
    fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        custom.join("claude_agent_sdk/__init__.py"),
        "raise ImportError('SDK unavailable')\n",
    )
    .unwrap();
    check("probe exited unsuccessfully");
}
