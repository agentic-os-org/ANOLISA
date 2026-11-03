//! Run component preparation from verified candidate bytes before file replacement.

use std::path::Component;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use anolisa_core::ComponentManifest;
use anolisa_core::install_runner::PreparedFileSet;
use anolisa_core::owned_executor::OwnedOpError;
use anolisa_platform::fs_layout::FsLayout;
use nix::sys::signal::{self, SaFlags, SigAction, SigHandler, SigSet, Signal};
use nix::unistd::Pid;

static INTERRUPTED: AtomicI32 = AtomicI32::new(0);
static PREPARATION: Mutex<()> = Mutex::new(());

extern "C" fn interrupted(value: i32) {
    INTERRUPTED.store(value, Ordering::Relaxed);
}

struct SignalGuard(Vec<(Signal, SigAction)>);

impl SignalGuard {
    fn install() -> Result<Self, OwnedOpError> {
        INTERRUPTED.store(0, Ordering::Relaxed);
        let mut guard = Self(Vec::new());
        let handler = SigAction::new(
            SigHandler::Handler(interrupted),
            SaFlags::empty(),
            SigSet::empty(),
        );
        for signal in [Signal::SIGINT, Signal::SIGTERM] {
            // The handler only stores an atomic value; the serialized caller
            // restores both process-wide handlers on every exit path.
            let old = unsafe { signal::sigaction(signal, &handler) }
                .map_err(|err| OwnedOpError(format!("preparation signal handler: {err}")))?;
            guard.0.push((signal, old));
        }
        Ok(guard)
    }
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        for (signal, old) in self.0.iter().rev() {
            // These are the unchanged handlers returned by sigaction above.
            let _ = unsafe { signal::sigaction(*signal, old) };
        }
    }
}

struct ProcessGroup(Child);

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // Give request-owned cleanup a bounded chance before killing the group.
        // A helper that already exited must not leave background downloaders.
        if let Ok(pid) = i32::try_from(self.0.id()) {
            let pid = Pid::from_raw(pid);
            if self.0.try_wait().ok().flatten().is_none() {
                let _ = signal::killpg(pid, Signal::SIGTERM);
                let deadline = Instant::now() + Duration::from_secs(60);
                while self.0.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            let _ = signal::killpg(pid, Signal::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

pub(super) fn run_candidate_prepare(
    manifest: &ComponentManifest,
    files: Option<&PreparedFileSet>,
    layout: &FsLayout,
) -> Result<(), OwnedOpError> {
    let Some(spec) = &manifest.prepare else {
        return Ok(());
    };
    let script = std::path::Path::new(&spec.script);
    if script.as_os_str().is_empty()
        || script
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || !(1..=3600).contains(&spec.timeout_secs)
    {
        return Err(OwnedOpError(
            "invalid candidate preparation path or deadline".into(),
        ));
    }
    let _lock = PREPARATION
        .lock()
        .map_err(|_| OwnedOpError("preparation lock poisoned".into()))?;
    let candidate = files
        .ok_or_else(|| OwnedOpError("candidate preparation requires verified files".into()))?
        .candidate_prefix(&layout.prefix)
        .map_err(|err| OwnedOpError(format!("candidate preparation staging: {err}")))?;
    let program = candidate.join(script);
    if !program.is_file() {
        return Err(OwnedOpError(
            "candidate preparation program is absent from verified files".into(),
        ));
    }
    let _signals = SignalGuard::install()?;
    let mut command = Command::new(program);
    command
        .arg("--prefix")
        .arg(
            layout
                .bin_dir
                .parent()
                .ok_or_else(|| OwnedOpError("layout has no binary prefix".into()))?,
        )
        .arg("--cache-dir")
        .arg(&layout.cache_dir)
        .current_dir(candidate)
        .stdin(Stdio::null())
        .env_clear();
    // Only installer/transport configuration crosses this boundary. Model
    // credentials and ambient Python/uv configuration cannot enter setup.
    for key in [
        "PATH",
        "HOME",
        "TMPDIR",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "no_proxy",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "DOCKER_CONFIG",
        "DOCKER_TLS_VERIFY",
        "DOCKER_CERT_PATH",
        "ANOLISA_SKILL_SCORE_SOURCE_DIR",
        "ANOLISA_SKILL_SCORE_OFFLINE",
        "ANOLISA_SKILL_SCORE_PYPI_INDEX",
        "ANOLISA_SKILL_SCORE_DEPENDENCIES_OFFLINE",
        "ANOLISA_SKILL_SCORE_BASE_IMAGE",
        "ANOLISA_SKILL_SCORE_BASE_RECIPE",
        "ANOLISA_SKILL_SCORE_BUILD_NETWORK",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut process = ProcessGroup(
        anolisa_core::process::spawn_retry_etxtbsy(&mut command)
            .map_err(|err| OwnedOpError(format!("candidate preparation spawn: {err}")))?,
    );
    let started = Instant::now();
    loop {
        if INTERRUPTED.load(Ordering::Relaxed) != 0 {
            return Err(OwnedOpError(
                "candidate preparation cancelled; prior installation retained".into(),
            ));
        }
        if let Some(status) = process
            .0
            .try_wait()
            .map_err(|err| OwnedOpError(format!("candidate preparation wait: {err}")))?
        {
            return if status.success() {
                Ok(())
            } else {
                Err(OwnedOpError(format!(
                    "candidate preparation failed ({status}); prior installation retained"
                )))
            };
        }
        if started.elapsed() >= Duration::from_secs(u64::from(spec.timeout_secs)) {
            return Err(OwnedOpError(
                "candidate preparation timed out; prior installation retained".into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;

    use anolisa_core::FileKind;
    use anolisa_core::install_runner::{InstallRunner, ResolvedInstallFile};

    use super::*;

    fn fixture(
        script: &[u8],
        timeout: u32,
    ) -> (
        tempfile::TempDir,
        FsLayout,
        ComponentManifest,
        PreparedFileSet,
    ) {
        let temporary = tempfile::tempdir().unwrap();
        let layout = FsLayout::system(Some(temporary.path().to_path_buf()));
        let artifact = temporary.path().join("fixture.tar.gz");
        let compressed = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut archive = tar::Builder::new(compressed);
        let mut header = tar::Header::new_gnu();
        header.set_size(script.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive
            .append_data(&mut header, "prepare.sh", script)
            .unwrap();
        fs::File::create(&artifact)
            .unwrap()
            .write_all(&archive.into_inner().unwrap().finish().unwrap())
            .unwrap();
        let destination = layout.lib_dir.join("fixture/prepare.sh");
        let files = InstallRunner::new(&layout)
            .prepare_replacement_files(
                "tar_gz",
                &artifact,
                &[ResolvedInstallFile {
                    source: Some("prepare.sh".into()),
                    dest: destination,
                    mode: Some("0755".into()),
                    kind: FileKind::Executable,
                    render: None,
                }],
            )
            .unwrap();
        let manifest = ComponentManifest::from_toml_str(&format!(
            r#"
[component]
name = "fixture"
version = "1.0.0"
[component.prepare]
script = "usr/local/lib/anolisa/fixture/prepare.sh"
timeout_secs = {timeout}
"#
        ))
        .unwrap();
        (temporary, layout, manifest, files)
    }

    #[test]
    fn preparation_runs_verified_candidate_and_keeps_installed_helper() {
        let (_temporary, layout, manifest, files) = fixture(
            b"#!/bin/sh\nset -eu\nmkdir -p \"$2\"\nprintf candidate > \"$2/prepared\"\n",
            10,
        );
        let installed = layout.lib_dir.join("fixture/prepare.sh");
        fs::create_dir_all(installed.parent().unwrap()).unwrap();
        fs::write(&installed, "old helper must not run").unwrap();
        run_candidate_prepare(&manifest, Some(&files), &layout).unwrap();
        assert_eq!(
            fs::read_to_string(layout.bin_dir.parent().unwrap().join("prepared")).unwrap(),
            "candidate"
        );
        assert_eq!(
            fs::read_to_string(installed).unwrap(),
            "old helper must not run"
        );
    }

    #[test]
    fn preparation_failure_and_timeout_are_not_success() {
        for (script, timeout, expected) in [
            (b"#!/bin/sh\nexit 7\n".as_slice(), 10, "failed"),
            (b"#!/bin/sh\nsleep 30 &\nwait\n".as_slice(), 1, "timed out"),
        ] {
            let (_temporary, layout, manifest, files) = fixture(script, timeout);
            let error = run_candidate_prepare(&manifest, Some(&files), &layout).unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
            assert!(!layout.lib_dir.join("fixture/prepare.sh").exists());
        }
    }

    #[test]
    fn absent_prepare_never_materializes_or_executes_dependencies() {
        let (_temporary, layout, mut manifest, _files) = fixture(b"#!/bin/sh\nexit 99\n", 10);
        manifest.prepare = None;
        run_candidate_prepare(&manifest, None, &layout).unwrap();
    }

    #[test]
    fn preparation_rejects_traversal_and_unmapped_script() {
        for path in ["../old-helper", "usr/local/bin/not-in-candidate"] {
            let (_temporary, layout, mut manifest, files) = fixture(b"#!/bin/sh\nexit 0\n", 10);
            manifest.prepare.as_mut().unwrap().script = path.into();
            assert!(run_candidate_prepare(&manifest, Some(&files), &layout).is_err());
        }
    }
}
