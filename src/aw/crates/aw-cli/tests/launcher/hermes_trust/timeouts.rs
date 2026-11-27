//! Hermes preflight Git and dotenv probes allow slow checkouts beyond 5 seconds.

use super::*;

fn assert_slow_preflight_succeeds(delay_probe: &str) {
    let mut fixture = Fixture::new();
    let profile = fixture.native_profile();
    let native = profile.parent().unwrap();
    let bin = native.join("bin");
    let source = native.join("source");
    let git_dir = source.join(".git");
    for directory in [&bin, &source, &git_dir] {
        fs::create_dir(directory).unwrap();
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let venv_config = native.join("pyvenv.cfg");
    fs::write(&venv_config, "include-system-site-packages = false\n").unwrap();
    fs::set_permissions(&venv_config, fs::Permissions::from_mode(0o600)).unwrap();

    let marker = fixture.root.join("delay-observed");
    let python = bin.join("python-fixture");
    fs::write(
        &python,
        format!(
            r#"#!{}
import json, os, sys, time
from pathlib import Path
source = Path(os.environ['TEST_SOURCE'])
marker = Path(os.environ['DELAY_MARKER'])
if '-I' in sys.argv:
    Path(sys.argv[-1]).write_text(json.dumps([str(source), False, []]))
elif any('load_hermes_dotenv' in arg for arg in sys.argv):
    if os.environ['DELAY_PROBE'] == 'dotenv' and not marker.exists():
        marker.touch()
        time.sleep(6)
    selected = [[list(os.fsencode(k)), list(os.fsencode(v))]
        for k, v in os.environ.items()
        if k in ('PATH', 'HOME', 'XDG_CONFIG_HOME') or k.startswith(('GIT_', 'LD_'))]
    Path(sys.argv[-1]).write_text(json.dumps(selected))
else:
    print('Install directory: ' + str(source))
"#,
            fixture.python.display()
        ),
    )
    .unwrap();
    let hermes = native.join("hermes");
    fs::write(
        &hermes,
        format!(
            "#!{}\n# from hermes_cli.main import main\n",
            python.display()
        ),
    )
    .unwrap();
    let git = native.join("git");
    fs::write(
        &git,
        format!(
            r#"#!{}
import os, sys, time
from pathlib import Path
source = Path(os.environ['TEST_SOURCE'])
marker = Path(os.environ['DELAY_MARKER'])
kind = (
    'fresh-status'
    if 'status' in sys.argv and 'GIT_INDEX_FILE' in os.environ
    else ('ls-files' if 'ls-files' in sys.argv else '')
)
if kind and os.environ['DELAY_PROBE'] == kind and not marker.exists():
    marker.touch()
    time.sleep(6)
if 'rev-parse' in sys.argv and sys.argv[-1] == 'HEAD':
    print('952c941e741e922a9be8fc403c8944c6e96318bb')
elif '--absolute-git-dir' in sys.argv:
    print(source / '.git')
elif '--git-path' in sys.argv:
    print(source / '.git' / sys.argv[-1])
elif 'config' in sys.argv and '--get' in sys.argv:
    print('false')
"#,
            fixture.python.display()
        ),
    )
    .unwrap();
    for file in [&python, &hermes, &git] {
        fs::set_permissions(file, fs::Permissions::from_mode(0o700)).unwrap();
    }

    let plugin = profile.join("plugins/aw-native-hooks");
    fs::create_dir_all(&plugin).unwrap();
    for directory in [profile.join("plugins"), plugin.clone()] {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let bundled =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../aw-adapter/native/hermes");
    for name in [
        "plugin.yaml",
        "__init__.py",
        "aw_gateway.py",
        "aw_gateway_attach.py",
        "aw_gateway_lifecycle.py",
        "aw_identity.py",
        "aw_result.py",
        "aw_sdk_attach.py",
        "aw_skill.py",
    ] {
        fs::copy(bundled.join(name), plugin.join(name)).unwrap();
        fs::set_permissions(plugin.join(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
    fs::write(plugin.join(".aw-owned"), "aw-hermes-plugin/v1alpha1\n").unwrap();
    fs::set_permissions(plugin.join(".aw-owned"), fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(
        profile.join("config.yaml"),
        "plugins: {enabled: [aw-native-hooks]}\nunknown: keep\n",
    )
    .unwrap();
    fs::set_permissions(
        profile.join("config.yaml"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();

    let mut document = fixture.document();
    document["spec"]["agents"] = json!({"hermes":{"adapter":"hermes","argv":[hermes,"chat"]}});
    fixture.save(&document);

    let mut command = fixture.command();
    command
        .current_dir(&source)
        .env("PATH", native)
        .env("HOME", native)
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("TMPDIR", native)
        .env("TEST_SOURCE", &source)
        .env("DELAY_PROBE", delay_probe)
        .env("DELAY_MARKER", &marker)
        .args(["connect", "--config"])
        .arg(fixture.root.join("aw.json"))
        .args(["--agent", "hermes", "--native-profile"])
        .arg(&profile);
    let output = Process::spawn(&fixture, command).finish_with_timeout(Duration::from_secs(25));
    assert!(
        output.status.success(),
        "{delay_probe} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(marker.exists(), "expected {delay_probe} delay to execute");
}

#[test]
fn hermes_preflight_allows_slow_fresh_index_git_status_beyond_five_seconds() {
    assert_slow_preflight_succeeds("fresh-status");
}

#[test]
fn hermes_preflight_allows_slow_git_inspect_beyond_five_seconds() {
    assert_slow_preflight_succeeds("ls-files");
}

#[test]
fn hermes_preflight_allows_slow_dotenv_probe_beyond_five_seconds() {
    assert_slow_preflight_succeeds("dotenv");
}
