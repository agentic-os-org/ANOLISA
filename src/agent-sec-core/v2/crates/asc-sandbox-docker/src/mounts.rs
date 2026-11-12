//! Pin authorized objects by descriptor and publish protected mounts without copying workspace data.

use crate::{ContainerPlan, DockerError};
use asc_sandbox_types::ExecutionSpec;
use rustix::fs::{Mode, OFlags, open, openat};
use rustix::mount::{MountPropagationFlags, MoveMountFlags, OpenTreeFlags, UnmountFlags};
use std::fs::{self, File};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
use std::path::{Component, Path, PathBuf};

pub(crate) const STAGING_ROOT: &str = "/run/asc-sandbox-docker";

pub(crate) struct PinnedMounts {
    directory: Option<tempfile::TempDir>,
    paths: Vec<PathBuf>,
}

impl PinnedMounts {
    pub(crate) fn prepare(
        spec: &ExecutionSpec,
        plan: &mut ContainerPlan,
    ) -> Result<Self, DockerError> {
        if !rustix::process::geteuid().is_root() {
            return Err(DockerError::Unsupported(
                "descriptor-pinned Docker mounts require host root and CAP_SYS_ADMIN".into(),
            ));
        }
        let root = trusted_root()?;
        let directory = tempfile::Builder::new().prefix("run-").tempdir_in(&root)?;
        let mut pinned = Self {
            directory: Some(directory),
            paths: Vec::new(),
        };
        let positions: Vec<_> = plan
            .create_args
            .iter()
            .enumerate()
            .filter_map(|(index, arg)| (arg == "--volume").then_some(index + 1))
            .collect();
        for (index, mount) in plan.mounts.iter_mut().enumerate() {
            let source = open_source(Path::new(&mount.source))?;
            let metadata = source.metadata()?;
            verify_source(spec, &mount.source, &metadata)?;
            let tree = rustix::mount::open_tree(
                &source,
                "",
                OpenTreeFlags::OPEN_TREE_CLONE
                    | OpenTreeFlags::OPEN_TREE_CLOEXEC
                    | OpenTreeFlags::AT_EMPTY_PATH
                    | OpenTreeFlags::AT_RECURSIVE,
            )
            .map_err(|error| {
                DockerError::Unsupported(format!(
                    "cannot pin bind source {}: {error}; require CAP_SYS_ADMIN and open_tree",
                    mount.source
                ))
            })?;
            let target = pinned
                .directory
                .as_ref()
                .ok_or_else(|| DockerError::Protocol("staging directory missing".into()))?
                .path()
                .join(index.to_string());
            if metadata.is_dir() {
                fs::create_dir(&target)?;
            } else {
                File::create_new(&target)?;
            }
            rustix::mount::move_mount(
                &tree,
                "",
                rustix::fs::CWD,
                &target,
                MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH,
            )
            .map_err(std::io::Error::from)?;
            // Track ownership before propagation setup can fail; never recursively delete a mounted workspace.
            pinned.paths.push(target.clone());
            rustix::mount::mount_change(
                &target,
                MountPropagationFlags::PRIVATE | MountPropagationFlags::REC,
            )
            .map_err(std::io::Error::from)?;
            let actual = fs::metadata(&target)?;
            if actual.dev() != metadata.dev() || actual.ino() != metadata.ino() {
                return Err(DockerError::Protocol(
                    "staging mount identity mismatch".into(),
                ));
            }
            mount.source = target
                .to_str()
                .ok_or_else(|| DockerError::Unsupported("non-UTF-8 staging path".into()))?
                .into();
            let position = *positions
                .get(index)
                .ok_or_else(|| DockerError::Protocol("missing bind argument".into()))?;
            let suffix = if mount.read_only { ":ro" } else { "" };
            plan.create_args[position] = format!("{}:{}{suffix}", mount.source, mount.destination);
        }
        Ok(pinned)
    }
}

impl Drop for PinnedMounts {
    fn drop(&mut self) {
        let mut failed = false;
        for path in self.paths.iter().rev() {
            if let Err(error) =
                rustix::mount::unmount(path, UnmountFlags::DETACH | UnmountFlags::NOFOLLOW)
            {
                eprintln!(
                    "docker staging cleanup unconfirmed at {}: {error}",
                    path.display()
                );
                failed = true;
            }
        }
        if failed && let Some(directory) = self.directory.take() {
            let _ = directory.keep();
        }
    }
}

fn verify_source(
    spec: &ExecutionSpec,
    source: &str,
    metadata: &fs::Metadata,
) -> Result<(), DockerError> {
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(DockerError::Unsupported(format!(
            "special bind source: {source}"
        )));
    }
    if let Some(fact) = spec.path_facts.iter().find(|fact| fact.path == source) {
        if fact.canonical_path != source
            || fact.device != metadata.dev()
            || fact.inode != metadata.ino()
            || fact.is_directory != metadata.is_dir()
        {
            return Err(DockerError::Unsupported(format!(
                "path identity changed: {source}"
            )));
        }
    } else if !spec.path_facts.is_empty() {
        return Err(DockerError::Unsupported(format!(
            "missing bound path identity: {source}"
        )));
    }
    Ok(())
}

fn trusted_root() -> Result<PathBuf, DockerError> {
    let run = open_source(Path::new("/run"))?;
    let metadata = run.metadata()?;
    if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(DockerError::Unsupported("untrusted /run directory".into()));
    }
    let path = PathBuf::from(STAGING_ROOT);
    match fs::DirBuilder::new().mode(0o700).create(&path) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.into()),
    }
    let file = open_source(&path)?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o077 != 0 {
        return Err(DockerError::Unsupported(
            "Docker staging root must be root-owned mode 0700".into(),
        ));
    }
    Ok(path)
}

fn open_source(path: &Path) -> Result<File, DockerError> {
    let flags = OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut file = File::from(
        open("/", flags | OFlags::DIRECTORY, Mode::empty()).map_err(std::io::Error::from)?,
    );
    let mut components = path.components().peekable();
    while let Some(component) = components.next() {
        match component {
            Component::RootDir => (),
            Component::Normal(name) => {
                let flags = if components.peek().is_some() {
                    flags | OFlags::DIRECTORY
                } else {
                    flags
                };
                file = File::from(openat(&file, name, flags, Mode::empty()).map_err(|error| {
                    DockerError::Unsupported(format!(
                        "cannot pin bind source {} without symlinks: {error}",
                        path.display()
                    ))
                })?);
            }
            _ => {
                return Err(DockerError::Unsupported(format!(
                    "invalid bind path: {}",
                    path.display()
                )));
            }
        }
    }
    if file.metadata()?.file_type().is_symlink() {
        return Err(DockerError::Unsupported(format!(
            "symlink bind source: {}",
            path.display()
        )));
    }
    Ok(file)
}
