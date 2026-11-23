//! Read literal installed versions for known console scripts; preserve custom probes.

use crate::{adapter::Probe, Result};
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
struct ConsoleProbe {
    version: Option<String>,
}

pub(super) fn inspect(
    command: &CommandSpec,
    cancelled: &AtomicBool,
) -> Result<(String, Option<String>)> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut managed_interpreter = None;
    if let Some((interpreter, source)) = candidate_interpreter(&command.program) {
        let mut minimal = command.clone();
        minimal.program = interpreter.clone().into();
        if let Ok(output) = Probe(cancelled).version_output_with_timeout(
            &minimal,
            &[
                "-c".into(),
                include_str!("../../../native/qwenpaw/version.py").into(),
                command.program.as_os_str().into(),
                source.into(),
            ],
            deadline.saturating_duration_since(Instant::now()),
        ) {
            if let Ok(console) = serde_json::from_str::<ConsoleProbe>(&output) {
                managed_interpreter = Some(interpreter);
                if let Some(version) = console.version {
                    return Ok((version, managed_interpreter));
                }
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
    let interpreter = header.strip_prefix("#!")?;
    let interpreter = if interpreter == "/bin/sh" {
        // UV resolves the console itself, not a symlink used to invoke it.
        body.strip_prefix(concat!(
            r#"'''exec' "$(dirname -- "$(realpath -- "$0")")"/'python' "$0" "$@""#,
            "\n' '''\n",
        ))?;
        program.canonicalize().ok()?.parent()?.join("python")
    } else {
        if !Path::new(interpreter).is_absolute() || interpreter.chars().any(char::is_whitespace) {
            return None;
        }
        interpreter.into()
    };
    Some((interpreter.to_str()?.to_owned(), source))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt};

    const ENTRY: &str = "import sys\nfrom qwenpaw.cli.main import cli\nif __name__ == '__main__':\n    sys.argv[0] = sys.argv[0].removesuffix('.exe')\n    sys.exit(cli())";

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
        let python = std::process::Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .unwrap();
        assert!(python.status.success());
        let interpreter = String::from_utf8(python.stdout).unwrap();
        let interpreter = interpreter.trim();
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
        let ordinary = include_str!("../../../tests/fixtures/qwenpaw/uv-console.py");
        let relocatable = include_str!("../../../tests/fixtures/qwenpaw/uv-relocatable");
        let setuptools = "import re\nimport sys\nfrom qwenpaw.cli.main import cli\nif __name__ == '__main__':\n    sys.argv[0] = re.sub(r'(-script\\.pyw|\\.exe)?$', '', sys.argv[0])\n    sys.exit(cli())\n";
        for (source, expected) in [
            (
                format!("#!{interpreter}\n{ordinary}"),
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
                format!("#!{interpreter}\n{setuptools}"),
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
        let python = std::process::Command::new("python3")
            .args([
                "-c",
                "import sys; assert sys.version_info >= (3, 11); print(sys.executable)",
            ])
            .output()
            .unwrap();
        assert!(python.status.success(), "AW tests require Python 3.11+");
        let python_path = String::from_utf8(python.stdout).unwrap();
        let python_path = python_path.trim();
        assert!(Path::new(python_path).is_absolute());
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
        let marker = directory.path().join("custom-entry-ran");
        fs::write(&program, format!("#!{python_path}\nfrom pathlib import Path\nPath({:?}).touch()\nprint('QwenPaw, version 9.1.0')\n", marker)).unwrap();
        assert_eq!(
            inspect(&command, &cancelled).unwrap(),
            ("QwenPaw, version 9.1.0".into(), None)
        );
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
