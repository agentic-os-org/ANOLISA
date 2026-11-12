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

const ENTRY: &str = "import sys\nfrom qwenpaw.cli.main import cli\nif __name__ == '__main__':\n    sys.argv[0] = sys.argv[0].removesuffix('.exe')\n    sys.exit(cli())";

pub(super) fn version(command: &CommandSpec, cancelled: &AtomicBool) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    if let Some(interpreter) = interpreter(&command.program) {
        let mut minimal = command.clone();
        minimal.program = interpreter.into();
        if let Ok(version) = Probe(cancelled).version_output_with_timeout(
            &minimal,
            &[
                "-c".into(),
                include_str!("../../../native/qwenpaw/version.py").into(),
                command.program.as_os_str().into(),
            ],
            deadline.saturating_duration_since(Instant::now()),
        ) {
            return Ok(version);
        }
    }
    Probe(cancelled).version_output_with_timeout(
        command,
        &["--version".into()],
        deadline.saturating_duration_since(Instant::now()),
    )
}

pub(super) fn interpreter(program: &Path) -> Option<String> {
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
    let source = std::str::from_utf8(&bytes).ok()?;
    let (header, body) = source.split_once('\n')?;
    let interpreter = header.strip_prefix("#!")?;
    (Path::new(interpreter).is_absolute()
        && !interpreter.chars().any(char::is_whitespace)
        && body.trim() == ENTRY)
        .then(|| interpreter.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt};

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
            version(&command, &cancelled).unwrap(),
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
            version(&command, &cancelled).unwrap(),
            "QwenPaw, version 2.2.0"
        );
        fs::write(&program, "#!/bin/sh\nprintf 'QwenPaw, version 9.0.0\\n'\n").unwrap();
        assert_eq!(
            version(&command, &cancelled).unwrap(),
            "QwenPaw, version 9.0.0"
        );
        fs::write(&program, format!("#!/usr/bin/env python3\n{ENTRY}\n")).unwrap();
        assert!(interpreter(&program).is_none());
    }
}
