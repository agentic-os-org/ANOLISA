//! Hermes probes and install writers release cancelled child processes.

use super::*;

#[test]
fn hermes_probes_and_install_writers_cancel_and_reap_on_signals() {
    use std::os::unix::fs::PermissionsExt;
    for probe in [
        "run-discovery",
        "run-version",
        "run-dotenv",
        "run-identity",
        "run-checkout",
        "run-tree",
        "run-content",
        "run-timeout",
        "install-discovery",
        "install-version",
        "install-dotenv",
        "install-identity",
        "install-checkout",
        "install-tree",
        "install-content",
        "plugins.disabled",
        "plugins.enabled",
    ] {
        for signal in [libc::SIGINT, libc::SIGTERM] {
            let mut fixture = Fixture::new();
            let profile = fixture.native_profile();
            let bin = profile.parent().unwrap().join("bin");
            fs::create_dir(&bin).unwrap();
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
            fs::create_dir(bin.join(".git")).unwrap();
            fs::set_permissions(bin.join(".git"), fs::Permissions::from_mode(0o700)).unwrap();
            let venv_config = profile.parent().unwrap().join("pyvenv.cfg");
            fs::write(&venv_config, "include-system-site-packages = false\n").unwrap();
            fs::set_permissions(venv_config, fs::Permissions::from_mode(0o600)).unwrap();
            let python = bin.join("python-fixture");
            fs::write(&python, format!(r#"#!{}
import json, os, sys, time
from pathlib import Path
root = Path(os.environ['FAKE_ROOT'])
selected = os.environ['HERMES_PROBE']
probe = selected if '-I' in sys.argv and selected.endswith('-discovery') else ('discovery' if '-I' in sys.argv else ('run-timeout' if '-c' in sys.argv else ('install-version' if selected == 'install-version' else 'run-version')))
if any('load_hermes_dotenv' in arg for arg in sys.argv):
    probe = selected if selected.endswith('-dotenv') else 'dotenv'
writer = len(sys.argv) > 2 and sys.argv[-1] in ('plugins.disabled', 'plugins.enabled')
if writer:
    probe = sys.argv[-1]
if probe == selected:
    (root / 'probe.pid.tmp').write_text(str(os.getpid()))
    (root / 'probe.pid.tmp').replace(root / 'probe.pid')
    time.sleep(90)
elif probe == 'dotenv':
    print(json.dumps([[list(os.fsencode(key)), list(os.fsencode(value))] for key, value in os.environ.items()]))
elif probe == 'discovery':
    print(json.dumps([str(Path(__file__).parent), []]))
elif writer:
    path = Path(sys.argv[-3]) / 'config.yaml'
    value = json.loads(path.read_text())
    value['plugins'][probe.split('.')[1]] = json.load(sys.stdin)
    path.write_text(json.dumps(value))
else:
    print('Install directory: ' + str(Path(__file__).parent))
"#, fixture.python.display())).unwrap();
            let hermes = profile.parent().unwrap().join("hermes");
            fs::write(
                &hermes,
                format!(
                    "#!{}\n# from hermes_cli.main import main\n",
                    python.display()
                ),
            )
            .unwrap();
            let git = profile.parent().unwrap().join("git");
            fs::write(
                &git,
                format!(
                    "#!{}\nimport os, sys, time\nfrom pathlib import Path\nprobe = '-identity' if 'rev-parse' in sys.argv and sys.argv[-1] == 'HEAD' else ('-tree' if 'read-tree' in sys.argv else ('-content' if 'status' in sys.argv and 'GIT_INDEX_FILE' in os.environ else '-checkout'))\nif os.environ['HERMES_PROBE'].endswith(probe):\n    root = Path(os.environ['FAKE_ROOT'])\n    (root / 'probe.pid.tmp').write_text(str(os.getpid()))\n    (root / 'probe.pid.tmp').replace(root / 'probe.pid')\n    time.sleep(90)\nif probe == '-identity':\n    print('952c941e741e922a9be8fc403c8944c6e96318bb')\nelif '--absolute-git-dir' in sys.argv:\n    print(Path(__file__).parent / 'bin/.git')\nelif '--git-path' in sys.argv:\n    print(Path(__file__).parent / 'bin/.git' / sys.argv[-1])\n",
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
                fs::set_permissions(directory, fs::Permissions::from_mode(0o755)).unwrap();
            }
            let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../aw-adapter/native/hermes");
            for name in [
                "plugin.yaml",
                "__init__.py",
                "aw_gateway.py",
                "aw_gateway_lifecycle.py",
                "aw_skill.py",
            ] {
                fs::copy(source.join(name), plugin.join(name)).unwrap();
                fs::set_permissions(plugin.join(name), fs::Permissions::from_mode(0o600)).unwrap();
            }
            fs::write(plugin.join(".aw-owned"), "aw-hermes-plugin/v1alpha1\n").unwrap();
            let installing = !probe.starts_with("run-");
            let original = if installing {
                r#"{"plugins":{"enabled":[],"disabled":["aw-native-hooks"]},"unknown":"keep"}"#
            } else {
                r#"{"plugins":{"enabled":["aw-native-hooks"]},"unknown":"keep"}"#
            };
            fs::write(profile.join("config.yaml"), original).unwrap();
            for file in [plugin.join(".aw-owned"), profile.join("config.yaml")] {
                fs::set_permissions(file, fs::Permissions::from_mode(0o600)).unwrap();
            }
            let mut document = fixture.document();
            document["spec"]["agents"] =
                json!({"hermes":{"adapter":"hermes","argv":[hermes,"chat"]}});
            fixture.save(&document);
            let mut command = fixture.command();
            let mut paths = vec![profile.parent().unwrap().to_path_buf()];
            paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
            command
                .current_dir(&bin)
                .env("PATH", std::env::join_paths(paths).unwrap())
                .env("HERMES_PROBE", probe)
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("TMPDIR", profile.parent().unwrap());
            command
                .args([if installing { "install" } else { "run" }, "--config"])
                .arg(fixture.root.join("aw.json"))
                .args(["--agent", "hermes", "--native-profile"])
                .arg(&profile);
            let process = Process::spawn(&fixture, command);
            let deadline = Instant::now() + TIMEOUT;
            while !fixture.root.join("probe.pid").exists() {
                assert!(
                    Instant::now() < deadline,
                    "Hermes {probe} probe did not start: {}",
                    fs::read_to_string(fixture.root.join("process-0.stderr")).unwrap()
                );
                thread::sleep(Duration::from_millis(5));
            }
            let pid: u32 = fs::read_to_string(fixture.root.join("probe.pid"))
                .unwrap()
                .parse()
                .expect("probe must publish a complete PID");
            let started = Instant::now();
            process.signal(signal);
            let output = process.finish();
            assert_eq!(output.status.signal(), Some(signal));
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("aw: ") && stderr.contains("command execution cancelled"),
                "interrupted {probe} discarded its error: {stderr}"
            );
            assert!(started.elapsed() < Duration::from_secs(3));
            assert!(
                !std::path::Path::new(&format!("/proc/{pid}")).exists(),
                "Hermes {probe} signal {signal} left PID {pid}"
            );
            assert_eq!(
                fs::read(profile.join("config.yaml")).unwrap(),
                original.as_bytes()
            );
            assert!(fs::read_dir(profile.parent().unwrap())
                .unwrap()
                .all(|entry| {
                    !entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with("aw-hermes-")
                }));
            assert!(fs::read_dir(&profile).unwrap().all(|entry| {
                let name = entry.unwrap().file_name();
                let name = name.to_str().unwrap();
                !name.starts_with(".aw-config-") && !name.starts_with("config.yaml.aw-backup-")
            }));
        }
    }
}
