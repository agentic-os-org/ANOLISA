//! Read literal installed versions for known console scripts; preserve custom probes.

use crate::{
    adapter::{isolate_python_environment, Probe},
    Result,
};
use aw_exec::CommandSpec;
use std::{
    fs::OpenOptions,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

#[derive(serde::Deserialize)]
#[serde(tag = "entry", rename_all = "lowercase", deny_unknown_fields)]
enum ConsoleProbe {
    Managed { version: Option<String> },
    Custom,
}

pub(super) fn inspect(
    command: &CommandSpec,
    cancelled: &AtomicBool,
) -> Result<(String, Option<String>)> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut managed_interpreter = None;
    if let Some((interpreter, source)) = candidate_interpreter(&command.program) {
        if let ConsoleProbe::Managed { version } =
            probe_console(command, &interpreter, source, deadline, cancelled)?
        {
            managed_interpreter = Some(interpreter);
            if let Some(version) = version {
                return Ok((version, managed_interpreter));
            }
        }
    }
    let version = Probe(cancelled).version_output_with_timeout(
        command,
        &["--version".into()],
        deadline.saturating_duration_since(Instant::now()),
    )?;
    Ok((version, managed_interpreter))
}

fn probe_console(
    command: &CommandSpec,
    interpreter: &str,
    source: String,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<ConsoleProbe> {
    // version.py reports EX_SOFTWARE even when it cannot write the result.
    const HELPER_FAILED: i32 = 70;
    let probe = Probe(cancelled);
    let mut inspection = command.clone();
    inspection.program = interpreter.into();
    isolate_python_environment(&mut inspection)?;
    inspection.args = vec![
        "-c".into(),
        include_str!("../../../native/qwenpaw/version.py").into(),
        command.program.as_os_str().into(),
        source.into(),
    ];
    let inspection = probe.run_python(
        inspection,
        &[],
        crate::adapter::PROBE_LIMITS,
        deadline,
        aw_exec::CleanupMode::Immediate,
    );
    // A script-only interpreter can exhaust either diagnostic stream with its
    // usage message. Only the native command can supply a custom launcher's version.
    let reports_python = || interpreter_reports_python(command, interpreter, &probe, deadline);
    let (output, payload) = match inspection {
        Err(error) if diagnostic_overflow(&error) && !reports_python()? => {
            return Ok(ConsoleProbe::Custom);
        }
        result => result?,
    };
    if output.status.success() && !payload.is_empty() {
        return Ok(serde_json::from_slice(&payload)?);
    }
    if payload.is_empty()
        && output
            .status
            .code()
            .is_some_and(|code| code != HELPER_FAILED)
        && !reports_python()?
    {
        return Ok(ConsoleProbe::Custom);
    }
    Err(if output.status.success() {
        "Python probe produced an empty result"
    } else {
        "Python preflight probe exited unsuccessfully"
    }
    .into())
}

fn interpreter_reports_python(
    command: &CommandSpec,
    interpreter: &str,
    probe: &Probe<'_>,
    deadline: Instant,
) -> Result<bool> {
    let mut identity = command.clone();
    identity.program = interpreter.into();
    identity.args = vec!["--version".into()];
    isolate_python_environment(&mut identity)?;
    let identity = match probe.run(&identity, deadline) {
        Err(error) if diagnostic_overflow(&error) => return Ok(false),
        result => result?,
    };
    Ok(identity.status.success()
        && identity.stdout.split(|&byte| byte == b'\n').any(|line| {
            line.strip_prefix(b"Python 3.")
                .and_then(|s| s.first())
                .is_some_and(u8::is_ascii_digit)
        }))
}

fn diagnostic_overflow(error: &crate::Error) -> bool {
    matches!(
        error,
        crate::Error::Exec(aw_exec::Error::OutputLimit {
            stream: aw_exec::Stream::Stdout | aw_exec::Stream::Stderr,
            ..
        })
    )
}

fn candidate_interpreter(program: &Path) -> Option<(String, String)> {
    if !program.is_absolute() {
        return None;
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(program)
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > 4096 {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 4096 {
        return None;
    }
    let source = String::from_utf8(bytes).ok()?;
    let (header, body) = source.split_once('\n')?;
    // Skip unrelated launchers. Allow spacing and continued lines in the module
    // name; only the Python AST can confirm a generated console.
    let compact: String = body
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\\')
        .collect();
    if !compact.contains("qwenpaw.cli.main") {
        return None;
    }
    let interpreter = header.strip_prefix("#!")?;
    let interpreter = if interpreter == "/bin/sh" {
        // UV resolves the console itself, not a symlink used to invoke it.
        body.strip_prefix(concat!(
            r#"'''exec' "$(dirname -- "$(realpath -- "$0")")"/'python' "$0" "$@""#,
            "\n' '''\n",
        ))?;
        program.canonicalize().ok()?.parent()?.join("python")
    } else {
        let path = Path::new(interpreter);
        if !path.is_absolute() || interpreter.chars().any(char::is_whitespace) {
            return None;
        }
        path.to_owned()
    };
    Some((interpreter.to_str()?.to_owned(), source))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, ffi::OsStr, fs, os::unix::fs::PermissionsExt};

    const ENTRY: &str = "import sys\nfrom qwenpaw.cli.main import cli\nif __name__ == '__main__':\n    sys.argv[0] = sys.argv[0].removesuffix('.exe')\n    sys.exit(cli())";

    const DISTLIB: &str = r#"# -*- coding: utf-8 -*-
import re
import sys
from qwenpaw.cli.main import cli
if __name__ == '__main__':
    sys.argv[0] = re.sub(r'(-script\.pyw|\.exe)?$', '', sys.argv[0])
    sys.exit(cli())
"#;

    fn python() -> std::path::PathBuf {
        let output = std::process::Command::new("python3")
            .args([
                "-c",
                "import sys; assert sys.version_info >= (3, 11); print(sys.executable)",
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "AW tests require Python 3.11+");
        String::from_utf8(output.stdout).unwrap().trim().into()
    }

    #[test]
    fn generated_consoles_share_the_lightweight_probe_and_managed_interpreter() {
        let directory = tempfile::tempdir().unwrap();
        let package = directory.path().join("qwenpaw");
        fs::create_dir(&package).unwrap();
        fs::write(
            package.join("__init__.py"),
            "raise RuntimeError('heavy import')",
        )
        .unwrap();
        fs::write(package.join("__version__.py"), "__version__ = '2.1.1b3'\n").unwrap();
        let python = python();
        let interpreter = python.to_str().unwrap();
        let bin = directory.path().join("runtime with spaces/bin");
        fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(interpreter, bin.join("python")).unwrap();
        let program = bin.join("qwenpaw");
        let link = directory.path().join("linked-console");
        std::os::unix::fs::symlink(&program, &link).unwrap();
        let command = CommandSpec {
            program: link,
            args: vec!["app".into()],
            cwd: directory.path().into(),
            environment: BTreeMap::from([("PYTHONPATH".into(), directory.path().into())]),
        };
        fs::write(
            directory.path().join("sitecustomize.py"),
            "print('QwenPaw startup banner')\n",
        )
        .unwrap();
        let ordinary = include_str!("../../../tests/fixtures/qwenpaw/uv-console.py");
        let relocatable = include_str!("../../../tests/fixtures/qwenpaw/uv-relocatable");
        for (source, expected) in [
            (
                format!("#!{interpreter}\n{ordinary}"),
                interpreter.to_owned(),
            ),
            (
                format!("#!{interpreter}\n\"\"\"Console entry.\"\"\"\n{ordinary}"),
                interpreter.to_owned(),
            ),
            (
                format!("#!{interpreter}\nr\"\"\"Console entry.\"\"\"\n{ordinary}"),
                interpreter.to_owned(),
            ),
            (
                format!(
                    "#!{interpreter}\n# installer comment\n{}",
                    ENTRY.replace("'", "\"")
                ),
                interpreter.to_owned(),
            ),
            (
                format!("#!{interpreter}\n{DISTLIB}"),
                interpreter.to_owned(),
            ),
            (
                relocatable.to_owned(),
                bin.join("python").to_str().unwrap().to_owned(),
            ),
        ] {
            fs::write(&program, source).unwrap();
            fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(
                inspect(&command, &AtomicBool::new(false)).unwrap(),
                ("QwenPaw, version 2.1.1b3".into(), Some(expected))
            );
        }
    }

    #[test]
    fn renamed_python_prepares_and_configures_the_distlib_console_for_managed_launch() {
        let root = tempfile::tempdir().unwrap();
        let interpreter = root.path().join("business-python");
        fs::copy(python(), &interpreter).unwrap();
        assert_managed_distlib_launch(root.path(), &interpreter, root.path().as_os_str());
    }

    fn assert_managed_distlib_launch(root: &Path, interpreter: &Path, pythonpath: &OsStr) {
        use super::super::{Adapter, LaunchContext, LaunchInput, QwenPaw};

        let console = root.join("qwenpaw-console");
        fs::write(&console, format!("#!{}\n{DISTLIB}", interpreter.display())).unwrap();
        fs::set_permissions(&console, fs::Permissions::from_mode(0o700)).unwrap();
        let package = root.join("qwenpaw");
        fs::create_dir(&package).unwrap();
        fs::write(
            package.join("__init__.py"),
            "raise RuntimeError('heavy import')",
        )
        .unwrap();
        fs::write(
            package.join("__version__.py"),
            "__version__ = '2.2.2.post1'",
        )
        .unwrap();

        let files = crate::test_support::Artifacts(root.join("launch"));
        fs::create_dir(&files.0).unwrap();
        let input = LaunchInput {
            document: serde_json::json!({"spec":{"events":{},"execution":{"default_event_budget_ms":5000}}}),
            target: "qwenpaw".into(),
            options: Default::default(),
            command: CommandSpec {
                program: console.clone(),
                args: vec!["app".into()],
                cwd: root.into(),
                environment: BTreeMap::from([
                    ("PYTHONPATH".into(), pythonpath.into()),
                    ("QWENPAW_WORKING_DIR".into(), root.into()),
                ]),
            },
        };
        let mut prepared = QwenPaw.prepare(input, &AtomicBool::new(false)).unwrap();
        let plan = prepared
            .configure(&LaunchContext {
                files: &files,
                binding_path: &files.0.join("binding.json"),
                executable: &root.join("aw"),
                steps: &[],
            })
            .unwrap();
        assert_eq!(
            plan.command.environment[OsStr::new("PYTHONPATH")],
            pythonpath
        );
        assert!(!plan
            .command
            .environment
            .contains_key(OsStr::new("OTEL_SDK_DISABLED")));
        assert_eq!(plan.command.program, interpreter);
        assert_eq!(plan.command.args[0], files.0.join("qwenpaw-launch.py"));
        assert_eq!(plan.command.args[1], console);
        assert_eq!(&plan.command.args[3..], &["app"]);
    }

    fn check_wrapped_console(
        hook_dir: &str,
        expect_hook: bool,
        wrapper_setup: &str,
        hook_setup: &str,
    ) {
        let root = tempfile::tempdir().unwrap();
        let interpreter = root.path().join("wrapped-python");
        fs::write(
            &interpreter,
            format!(
                "#!{}\nimport os, sys\nwith open('wrapper-calls', 'a') as f: print(sys.argv[1], file=f)\n{wrapper_setup}\nos.execv(sys.executable, [sys.executable, *sys.argv[1:]])\n",
                python().display()
            ),
        )
        .unwrap();
        fs::set_permissions(&interpreter, fs::Permissions::from_mode(0o700)).unwrap();
        let hooks = root.path().join(hook_dir);
        fs::create_dir_all(&hooks).unwrap();
        fs::write(
            hooks.join("sitecustomize.py"),
            format!("from pathlib import Path\n{hook_setup}\nPath('hook-ran').touch()\nprint('startup banner', flush=True)\n"),
        )
        .unwrap();
        let pythonpath = std::env::join_paths([hooks.as_path(), root.path()]).unwrap();
        assert_managed_distlib_launch(root.path(), &interpreter, &pythonpath);
        assert_eq!(root.path().join("hook-ran").exists(), expect_hook);
        assert_eq!(
            fs::read_to_string(root.path().join("wrapper-calls")).unwrap(),
            "-c\n"
        );
    }

    #[test]
    fn wrapped_python_accepts_startup_banners() {
        check_wrapped_console("custom", true, "", "");
    }

    #[test]
    fn wrapped_python_skips_opentelemetry_startup() {
        check_wrapped_console(
            "opentelemetry/instrumentation/auto_instrumentation",
            false,
            "",
            "",
        );
    }

    #[test]
    fn slow_wrapped_python_uses_one_console_probe() {
        check_wrapped_console("custom", true, "", "import time; time.sleep(1.8)");
    }

    #[test]
    fn wrapped_python_can_report_its_own_version() {
        check_wrapped_console(
            "custom",
            true,
            "if sys.argv[1:] == ['--version']:\n    print('wrapper 1.0'); sys.exit(0)",
            "",
        );
    }

    #[test]
    fn custom_interpreter_usage_preserves_native_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let interpreter = directory.path().join("script-only");
        let console = directory.path().join("console");
        fs::write(
            &console,
            format!("#!{}\n# qwenpaw.cli.main\n", interpreter.display()),
        )
        .unwrap();
        fs::set_permissions(&console, fs::Permissions::from_mode(0o700)).unwrap();
        let command = CommandSpec {
            program: console,
            args: vec!["app".into()],
            cwd: directory.path().into(),
            environment: BTreeMap::new(),
        };
        for (usage, identity) in [
            ("printf '%04100d' 0 >&2", "echo 'script runner 1.0'"),
            ("printf '%04100d' 0", "printf '%04100d' 0 >&2"),
        ] {
            fs::write(
                &interpreter,
                format!(
                    r#"#!/bin/sh
case "$1" in
    -c) {usage}; exit 2 ;;
    --version) {identity} ;;
    *) test "$2" = --version && echo 'QwenPaw, version 2.2.2.post1' ;;
esac
"#
                ),
            )
            .unwrap();
            fs::set_permissions(&interpreter, fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(
                inspect(&command, &AtomicBool::new(false)).unwrap(),
                ("QwenPaw, version 2.2.2.post1".into(), None)
            );
        }
    }

    #[test]
    fn generated_console_rejects_missing_and_unwritable_probe_results() {
        let directory = tempfile::tempdir().unwrap();
        let interpreter = directory.path().join("interpreter");
        let program = directory.path().join("qwenpaw");
        fs::write(&program, format!("#!{}\n{ENTRY}\n", interpreter.display())).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        let command = CommandSpec {
            program,
            args: Vec::new(),
            cwd: directory.path().into(),
            environment: BTreeMap::new(),
        };
        for (identity, probe, expected) in [
            (
                "exec python3 --version",
                "echo 'stdout banner'",
                "empty result",
            ),
            (
                "exec python3 --version",
                "printf '%04100d' 0 >&2",
                "Stderr exceeded byte limit",
            ),
            (
                "echo 'wrapper 1.0'",
                r#"printf '%04100d' 0 > "$5""#,
                "Result exceeded byte limit",
            ),
            (
                "echo 'wrapper 1.0'",
                r#"exec python3 "$1" "$2" "$3" "$4" /dev/full"#,
                "exited unsuccessfully",
            ),
        ] {
            fs::write(
                &interpreter,
                format!(
                    r#"#!/bin/sh
case "$1" in
    --version) {identity} ;;
    -c) {probe} ;;
    *) touch native-version; echo 'QwenPaw, version 2.1.1' ;;
esac
"#
                ),
            )
            .unwrap();
            fs::set_permissions(&interpreter, fs::Permissions::from_mode(0o700)).unwrap();
            let error = inspect(&command, &AtomicBool::new(false)).unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
            assert!(!directory.path().join("native-version").exists());
        }
    }

    #[test]
    fn literal_version_avoids_package_import_and_custom_entries_keep_their_probe() {
        let directory = tempfile::tempdir().unwrap();
        let package = directory.path().join("qwenpaw");
        fs::create_dir(&package).unwrap();
        fs::write(
            package.join("__init__.py"),
            "raise RuntimeError('heavy import')",
        )
        .unwrap();
        fs::write(package.join("__version__.py"), "__version__ = '2.1.1b3'\n").unwrap();
        let program = directory.path().join("qwenpaw-cli");
        let python = python();
        let python_path = python.to_str().unwrap();
        fs::write(&program, format!("#!{python_path}\n{ENTRY}\n")).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        let command = CommandSpec {
            program: program.clone(),
            args: vec!["app".into()],
            cwd: directory.path().into(),
            environment: BTreeMap::from([("PYTHONPATH".into(), directory.path().into())]),
        };
        let cancelled = AtomicBool::new(false);
        assert_eq!(
            inspect(&command, &cancelled).unwrap().0,
            "QwenPaw, version 2.1.1b3"
        );
        fs::write(package.join("__init__.py"), "").unwrap();
        fs::write(
            package.join("__version__.py"),
            "__version__ = '.'.join(['2', '2', '0'])\n",
        )
        .unwrap();
        fs::create_dir(package.join("cli")).unwrap();
        fs::write(package.join("cli/__init__.py"), "").unwrap();
        fs::write(package.join("cli/main.py"), "from ..__version__ import __version__\ndef cli(): print('QwenPaw, version ' + __version__)\n").unwrap();
        assert_eq!(
            inspect(&command, &cancelled).unwrap(),
            (
                "QwenPaw, version 2.2.0".into(),
                Some(python_path.to_owned())
            )
        );
        let encoded_marker = directory.path().join("encoded-entry-ran");
        fs::write(
            &program,
            format!(
                "#!{python_path}\n# coding: unicode_escape# \\x0a__import__('pathlib').Path({:?}).touch()\n{ENTRY}\n",
                encoded_marker.display().to_string()
            ),
        )
        .unwrap();
        assert_eq!(
            inspect(&command, &cancelled).unwrap(),
            ("QwenPaw, version 2.2.0".into(), None)
        );
        assert!(!encoded_marker.exists());
        let execution = std::process::Command::new(python_path)
            .args([
                "-c",
                "import runpy, sys; runpy.run_path(sys.argv[1], run_name='__main__')",
            ])
            .arg(&program)
            .env("PYTHONPATH", directory.path())
            .output()
            .unwrap();
        assert!(execution.status.success());
        assert!(encoded_marker.exists());
        fs::write(
            &program,
            r#"#!/bin/bash
"/usr/bin/test" "$1" = --version && echo 'QwenPaw, version 2.2.2.post1'
"#,
        )
        .unwrap();
        assert!(candidate_interpreter(&program).is_none());
        assert_eq!(
            inspect(&command, &cancelled).unwrap(),
            ("QwenPaw, version 2.2.2.post1".into(), None)
        );
        for shell in ["/bin/bash", "/bin/sh", "/bin/dash", "/bin/zsh", "/bin/ksh"]
            .into_iter()
            .filter(|path| Path::new(path).is_file())
        {
            fs::write(
                &program,
                format!("#!{shell}\n# qwenpaw.cli.main\necho 'QwenPaw, version 2.2.2.post1'\n"),
            )
            .unwrap();
            assert_eq!(
                inspect(&command, &cancelled).unwrap(),
                ("QwenPaw, version 2.2.2.post1".into(), None)
            );
        }
        let marker = directory.path().join("custom-entry-ran");
        fs::write(&program, format!("#!{python_path}\nfrom pathlib import Path\nPath({:?}).touch()\nprint('QwenPaw, version 9.1.0')\n", marker)).unwrap();
        assert_eq!(
            inspect(&command, &cancelled).unwrap(),
            ("QwenPaw, version 9.1.0".into(), None)
        );
        assert!(candidate_interpreter(&program).is_none());
        assert!(marker.exists());
        fs::write(&program, "#!/bin/sh\nprintf 'QwenPaw, version 9.0.0\\n'\n").unwrap();
        assert_eq!(
            inspect(&command, &cancelled).unwrap(),
            ("QwenPaw, version 9.0.0".into(), None)
        );
        fs::write(&program, format!("#!/usr/bin/env python3\n{ENTRY}\n")).unwrap();
        assert!(candidate_interpreter(&program).is_none());
    }
}
