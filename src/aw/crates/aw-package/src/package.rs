//! Build a clean pinned checkout and stage matching native component archives.

use crate::{digest, require, Component, Error, Manifest, Payload, Result};
use flate2::{write::GzEncoder, Compression};
use std::{
    collections::BTreeMap,
    fs::{self, File, Permissions},
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

struct Cancellable<'a, W> {
    inner: W,
    cancel: &'a AtomicBool,
}

impl<W: Write> Write for Cancellable<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        check_cancel(self.cancel)?;
        self.inner.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        check_cancel(self.cancel)?;
        self.inner.flush()
    }
}

fn check_cancel(cancel: &AtomicBool) -> std::io::Result<()> {
    if cancel.load(Ordering::Relaxed) {
        // Interrupted is retried by io::copy/write_all; cancellation must stop them.
        Err(std::io::Error::other("build interrupted"))
    } else {
        Ok(())
    }
}

fn copy(source: &Path, target: &Path, cancel: &AtomicBool) -> Result<()> {
    check_cancel(cancel)?;
    let mut output = Cancellable {
        inner: File::create(target)?,
        cancel,
    };
    std::io::copy(&mut File::open(source)?, &mut output)?;
    fs::set_permissions(target, source.metadata()?.permissions())?;
    check_cancel(cancel)?;
    Ok(())
}

fn command(
    repo: &Path,
    program: &str,
    args: &[String],
    seconds: u64,
    cancel: &AtomicBool,
) -> Result<Vec<u8>> {
    eprintln!("+ {program} {}", args.join(" "));
    let output = aw_exec::run(
        &aw_exec::CommandSpec {
            program: program.into(),
            args: args.iter().map(Into::into).collect(),
            cwd: repo.into(),
            environment: std::env::vars_os().collect(),
        },
        b"",
        aw_exec::Limits {
            input_bytes: 0,
            stdout_bytes: 64 * 1024 * 1024,
            stderr_bytes: 64 * 1024 * 1024,
        },
        Instant::now() + Duration::from_secs(seconds),
        cancel,
    )
    .map_err(|error| Error::Invalid(format!("{program}: {error}")))?;
    std::io::stderr().write_all(&output.stderr)?;
    require(
        output.status.success(),
        format!("{program} exited with {}", output.status),
    )?;
    Ok(output.stdout)
}

fn valid_version(version: &str) -> bool {
    let Some((release, preview)) = version.split_once("-preview.") else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    release.split('.').count() == 3 && release.split('.').all(digits) && digits(preview)
}

fn elf(path: &Path) -> Result<()> {
    let mut header = [0; 20];
    File::open(path)?.read_exact(&mut header)?;
    let expected = match std::env::consts::ARCH {
        "x86_64" => 62,
        "aarch64" => 183,
        _ => 0,
    };
    require(
        &header[..6] == b"\x7fELF\x02\x01"
            && u16::from_le_bytes([header[18], header[19]]) == expected
            && path.metadata()?.permissions().mode() & 0o111 != 0,
        format!("expected native executable ELF: {}", path.display()),
    )
}

/// Explicit distribution selection; core never reads or builds sec-core payloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection {
    /// AW CLI and package manager only.
    Core,
    /// The sec-core extension, requiring a separately installed matching core.
    SecCore,
    /// Core, sec-core extension and their identical combined package.
    All,
}

impl Selection {
    pub(super) fn components(self) -> &'static [&'static str] {
        match self {
            Self::Core => &["aw-core"],
            Self::SecCore => &["aw-provider-sec-core"],
            Self::All => &["aw-core", "aw-provider-sec-core"],
        }
    }

    fn archives(self) -> &'static [&'static str] {
        match self {
            Self::Core => &["aw-core"],
            Self::SecCore => &["aw-provider-sec-core"],
            Self::All => &["aw-core", "aw-provider-sec-core", "aw-all-in-one"],
        }
    }

    fn builds(self, repo: &Path) -> Vec<(PathBuf, Vec<&'static str>)> {
        let mut aw = vec!["aw-package"];
        if self != Self::SecCore {
            aw.push("aw-service");
        }
        if self != Self::Core {
            aw.push("aw-provider-sec-core");
        }
        let mut builds = vec![(repo.join("src/aw"), aw)];
        if self != Self::Core {
            builds.push((
                repo.join("src/agent-sec-core/v2"),
                vec!["asc-cli", "asc-daemon"],
            ));
        }
        builds
    }
}

/// Build the selected Rust packages with locked dependencies, then archive them.
/// Requires a clean Git checkout and an empty absolute output directory.
/// Cancellation reaps only this build's owned subprocess groups.
pub fn build(
    repo: &Path,
    version: &str,
    output: &Path,
    cancel: &AtomicBool,
    selection: Selection,
) -> Result<Vec<PathBuf>> {
    require(
        std::env::consts::OS == "linux" && matches!(std::env::consts::ARCH, "x86_64" | "aarch64"),
        "Preview builds require native Linux x86_64 or aarch64",
    )?;
    require(valid_version(version), "version must be X.Y.Z-preview.N")?;
    require(output.is_absolute(), "output must be absolute")?;
    let git = |args: &[&str]| {
        command(
            repo,
            "git",
            &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            10,
            cancel,
        )
    };
    require(
        git(&["status", "--porcelain", "--untracked-files=normal"])?.is_empty(),
        "commit the source tree before building a Preview",
    )?;
    let source = String::from_utf8(git(&["rev-parse", "HEAD"])?)
        .map_err(|e| Error::Invalid(e.to_string()))?
        .trim()
        .to_string();
    fs::create_dir_all(output)?;
    require(
        fs::read_dir(output)?.next().is_none(),
        "output directory must be empty",
    )?;
    for (directory, packages) in selection.builds(repo) {
        let mut args: Vec<String> = [
            "+1.97.1",
            "build",
            "--release",
            "--locked",
            "--bins",
            "--target-dir",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        args.push(directory.join("target").display().to_string());
        for package in packages {
            args.extend(["-p".into(), package.into()]);
        }
        command(&directory, "cargo", &args, 1200, cancel)?;
    }
    require(
        git(&["status", "--porcelain", "--untracked-files=normal"])?.is_empty()
            && String::from_utf8_lossy(&git(&["rev-parse", "HEAD"])?).trim() == source,
        "source checkout changed during the build",
    )?;
    stage(repo, version, &source, output, cancel, selection)
}

fn add(
    root: &Path,
    name: &str,
    source: &Path,
    executable: bool,
    files: &mut BTreeMap<String, Payload>,
    cancel: &AtomicBool,
) -> Result<()> {
    check_cancel(cancel)?;
    if executable {
        elf(source)?;
    }
    let target = root.join("payload").join(name);
    fs::create_dir_all(
        target
            .parent()
            .ok_or_else(|| Error::Invalid("payload has no parent".into()))?,
    )?;
    copy(source, &target, cancel)?;
    let mode = if executable { 0o755 } else { 0o644 };
    fs::set_permissions(&target, Permissions::from_mode(mode))?;
    files.insert(
        name.into(),
        Payload {
            sha256: digest(&target)?,
            mode,
        },
    );
    Ok(())
}

fn stage(
    repo: &Path,
    version: &str,
    source: &str,
    output: &Path,
    cancel: &AtomicBool,
    selection: Selection,
) -> Result<Vec<PathBuf>> {
    check_cancel(cancel)?;
    let stage = tempfile::Builder::new()
        .prefix("stage-")
        .tempdir_in(output)?;
    let aw = repo.join("src/aw/target/release");
    let sec = repo.join("src/agent-sec-core/v2/target/release");
    let mut components = Vec::new();
    for name in selection.components() {
        let root = stage.path().join(name);
        let mut files = BTreeMap::new();
        if *name == "aw-core" {
            for binary in ["aw", "aw-package"] {
                add(
                    &root,
                    &format!("bin/{binary}"),
                    &aw.join(binary),
                    true,
                    &mut files,
                    cancel,
                )?;
            }
        } else {
            for (binary, directory) in [
                ("aw-provider-sec-core", &aw),
                ("agent-sec-cli", &sec),
                ("agent-sec-daemon", &sec),
            ] {
                add(
                    &root,
                    &format!("libexec/aw/providers/sec-core/{binary}"),
                    &directory.join(binary),
                    true,
                    &mut files,
                    cancel,
                )?;
            }
        }
        for notice in ["LICENSE", "NOTICE"] {
            add(
                &root,
                &format!("share/doc/{name}/{notice}"),
                &repo.join(notice),
                false,
                &mut files,
                cancel,
            )?;
        }
        components.push(Component {
            format: 1,
            component: (*name).into(),
            version: version.into(),
            source_commit: source.into(),
            os: "linux".into(),
            arch: std::env::consts::ARCH.into(),
            provider_protocol: "aw-provider/v1alpha1".into(),
            requires: if *name == "aw-core" {
                BTreeMap::new()
            } else {
                BTreeMap::from([("aw-core".into(), version.into())])
            },
            files,
        });
    }
    let mut archives = Vec::new();
    for name in selection.archives() {
        let selected: Vec<_> = components
            .iter()
            .filter(|component| *name == "aw-all-in-one" || component.component == *name)
            .cloned()
            .collect();
        let filename = format!("{name}-{version}-linux-{}", std::env::consts::ARCH);
        let root = stage.path().join(&filename);
        fs::create_dir(&root)?;
        for component in &selected {
            for path in component.files.keys() {
                let destination = root.join("payload").join(path);
                fs::create_dir_all(
                    destination
                        .parent()
                        .ok_or_else(|| Error::Invalid("payload has no parent".into()))?,
                )?;
                copy(
                    &stage
                        .path()
                        .join(&component.component)
                        .join("payload")
                        .join(path),
                    &destination,
                    cancel,
                )?;
            }
        }
        fs::write(
            root.join("manifest.json"),
            serde_json::to_vec_pretty(&Manifest {
                format: 1,
                components: selected,
            })?,
        )?;
        copy(&aw.join("aw-package"), &root.join("aw-package"), cancel)?;
        for (language, name) in [("en", "README.md"), ("zh", "README_zh.md")] {
            let text = fs::read_to_string(repo.join(format!(
                "docs/user-guide/{language}/user-entrypoint/aw-preview.md"
            )))?
            .replace("../../zh/user-entrypoint/aw-preview.md", "README_zh.md")
            .replace("../../en/user-entrypoint/aw-preview.md", "README.md");
            fs::write(root.join(name), text)?;
        }
        let destination = stage.path().join(format!("{filename}.tar.gz"));
        let mut archive = tar::Builder::new(Cancellable {
            inner: GzEncoder::new(File::create_new(&destination)?, Compression::default()),
            cancel,
        });
        archive.append_dir_all(&filename, &root)?;
        archive.into_inner()?.inner.finish()?;
        check_cancel(cancel)?;
        archives.push(destination);
    }
    let mut checksums = File::create_new(stage.path().join("SHA256SUMS"))?;
    for archive in &archives {
        check_cancel(cancel)?;
        writeln!(
            checksums,
            "{}  {}",
            digest(archive)?,
            archive
                .file_name()
                .ok_or_else(|| Error::Invalid("archive has no name".into()))?
                .to_string_lossy()
        )?;
    }
    archives.push(stage.path().join("SHA256SUMS"));
    let mut published = publish_using(&archives, output, cancel, |source, target| {
        fs::hard_link(source, target)
    })?;
    published.pop();
    Ok(published)
}

fn publish_using(
    artifacts: &[PathBuf],
    output: &Path,
    cancel: &AtomicBool,
    mut link: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<Vec<PathBuf>> {
    let mut published = Vec::new();
    let result = (|| -> Result<()> {
        for artifact in artifacts {
            check_cancel(cancel)?;
            let destination = output.join(
                artifact
                    .file_name()
                    .ok_or_else(|| Error::Invalid("archive has no name".into()))?,
            );
            link(artifact, &destination)?;
            published.push(destination);
        }
        check_cancel(cancel)?;
        Ok(())
    })();
    if let Err(error) = result {
        for path in published.iter().rev() {
            if let Err(cleanup) = fs::remove_file(path) {
                eprintln!(
                    "aw-build: rollback could not remove {}: {cleanup}",
                    path.display()
                );
            }
        }
        return Err(error);
    }
    Ok(published)
}

#[cfg(test)]
mod tests;
