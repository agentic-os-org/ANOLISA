//! Descriptor-relative filesystem access confined to the session workspace.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use rustix::fs::{mkdirat, open, openat, readlinkat, statat, AtFlags};
#[cfg(target_os = "linux")]
use rustix::fs::{mkdirat, openat2, readlinkat, ResolveFlags, CWD};
use rustix::fs::{Dir, FileType, Mode, OFlags};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use super::expand_tilde;

const MAX_WALK_ENTRIES: usize = 10_000;
const MAX_IGNORE_FILE_BYTES: u64 = 1024 * 1024;
const MAX_IGNORE_TOTAL_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SYMLINKS: usize = 40;
#[cfg(target_os = "linux")]
const RESOLVE_FLAGS: ResolveFlags = ResolveFlags::BENEATH
    .union(ResolveFlags::NO_MAGICLINKS)
    .union(ResolveFlags::NO_XDEV);
#[cfg(target_os = "linux")]
const ROOT_RESOLVE_FLAGS: ResolveFlags = ResolveFlags::NO_MAGICLINKS;
#[cfg(target_os = "linux")]
const PIN_FLAGS: OFlags = OFlags::PATH.union(OFlags::CLOEXEC).union(OFlags::NOFOLLOW);
const READ_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);

#[derive(Debug)]
enum WorkspaceOpenError {
    Escape(PathBuf),
    Inaccessible(PathBuf),
    SymlinkLoop(PathBuf),
    Unsupported(PathBuf),
    Other(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkspaceNodeKind {
    File,
    Directory,
}

pub(super) enum WorkspaceRootError {
    Missing(String),
    Permanent(String),
}

impl fmt::Display for WorkspaceRootError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(message) | Self::Permanent(message) => formatter.write_str(message),
        }
    }
}

impl fmt::Display for WorkspaceOpenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Escape(path) => {
                write!(formatter, "Path escapes workspace root: {}", path.display())
            }
            Self::Inaccessible(path) => {
                write!(formatter, "Permission denied: {}", path.display())
            }
            Self::SymlinkLoop(path) => {
                write!(
                    formatter,
                    "Too many symbolic links while opening {}",
                    path.display()
                )
            }
            Self::Unsupported(path) => {
                write!(
                    formatter,
                    "Unsupported filesystem object: {}",
                    path.display()
                )
            }
            Self::Other(message) => formatter.write_str(message),
        }
    }
}

/// A regular file opened beneath the workspace root.
#[derive(Debug)]
pub(super) struct WorkspaceFile {
    pub relative_path: PathBuf,
    pub display_path: PathBuf,
    pub file: File,
}

/// A write target whose parent directory is pinned by descriptor.
pub(super) struct WorkspaceWriteTarget {
    pub parent: File,
    pub name: OsString,
    pub display_path: PathBuf,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
enum ResolveMode {
    Readable,
    Pinned,
    Write {
        create_parents: bool,
        must_be_directory: bool,
    },
}

#[cfg(target_os = "linux")]
enum ResolvedBeneath {
    Node(Option<File>),
    Write(WorkspaceWriteTarget),
}

/// A directory opened beneath the workspace root.
#[derive(Debug)]
pub(super) struct WorkspaceDirectory {
    pub relative_path: PathBuf,
    pub display_path: PathBuf,
    pub file: File,
}

/// An opened regular file or directory.
pub(super) enum WorkspaceNode {
    File(WorkspaceFile),
    Directory(WorkspaceDirectory),
}

/// An exact path discovered for a multi-file read.
pub(super) enum WorkspaceBatchNode {
    Node(WorkspaceNode),
    InaccessibleFile {
        display_path: PathBuf,
        error: String,
    },
}

/// A metadata-only regular-file path or the result of opening an exact directory.
pub(super) enum WorkspacePathNode {
    File(PathBuf),
    Directory(WorkspaceDirectory),
    InaccessibleDirectory {
        display_path: PathBuf,
        error: String,
    },
}

/// Bounded file discovery results.
pub(super) struct WalkedFiles {
    pub files: Vec<WorkspaceFile>,
    pub truncated: bool,
}

/// Bounded metadata-only file discovery results.
pub(super) struct WalkedFilePaths {
    pub paths: Vec<PathBuf>,
    pub truncated: bool,
}

/// Bounded workspace-relative metadata-only file discovery results.
pub(super) struct WalkedRelativeFilePaths {
    pub paths: Vec<PathBuf>,
    pub truncated: bool,
    pub ignore_incomplete: bool,
}

#[derive(Clone, Default)]
struct IgnoreMatchers {
    git_exclude: Vec<Gitignore>,
    gitignore: Vec<Gitignore>,
    ignore: Vec<Gitignore>,
    rgignore: Vec<Gitignore>,
}

impl IgnoreMatchers {
    fn extend(&mut self, loaded: LoadedIgnoreMatchers) {
        self.git_exclude.extend(loaded.git_exclude);
        self.gitignore.extend(loaded.gitignore);
        self.ignore.extend(loaded.ignore);
        self.rgignore.extend(loaded.rgignore);
    }
}

#[derive(Default)]
struct LoadedIgnoreMatchers {
    git_exclude: Option<Gitignore>,
    gitignore: Option<Gitignore>,
    ignore: Option<Gitignore>,
    rgignore: Option<Gitignore>,
    incomplete: bool,
}

struct LoadedIgnoreFile {
    matcher: Option<Gitignore>,
    incomplete: bool,
}

struct GitMarker {
    is_directory: bool,
}

/// Pins a trusted workspace root and resolves untrusted paths beneath it.
pub(super) struct WorkspaceFs {
    root: PathBuf,
    requested_root: PathBuf,
    directory: File,
}

impl WorkspaceFs {
    pub fn new(root: &Path) -> Result<Self, String> {
        Self::open_root(root).map_err(|error| error.to_string())
    }

    #[cfg(target_os = "linux")]
    pub(super) fn open_root(root: &Path) -> Result<Self, WorkspaceRootError> {
        let requested_root = root.to_path_buf();
        let descriptor = openat2(
            CWD,
            root,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            ROOT_RESOLVE_FLAGS,
        )
        .map_err(|error| {
            if error == rustix::io::Errno::NOSYS {
                return platform_support_error(error);
            }
            let message = format!("Failed to open workspace root {}: {error}", root.display());
            if error == rustix::io::Errno::NOENT {
                WorkspaceRootError::Missing(message)
            } else {
                WorkspaceRootError::Permanent(message)
            }
        })?;
        let directory = File::from(descriptor);
        Self::check_platform_support(&directory)?;
        let root = root_path_from_descriptor(&directory, root)?;
        Ok(Self {
            root,
            requested_root,
            directory,
        })
    }

    #[cfg(target_os = "macos")]
    pub(super) fn open_root(root: &Path) -> Result<Self, WorkspaceRootError> {
        let requested_root = root.to_path_buf();
        let root = root.canonicalize().map_err(|error| {
            let message = format!("Failed to open workspace root {}: {error}", root.display());
            if error.kind() == std::io::ErrorKind::NotFound {
                WorkspaceRootError::Missing(message)
            } else {
                WorkspaceRootError::Permanent(message)
            }
        })?;
        let directory = File::open(&root).map_err(|error| {
            WorkspaceRootError::Permanent(format!(
                "Failed to open workspace root {}: {error}",
                root.display()
            ))
        })?;
        let metadata = directory.metadata().map_err(|error| {
            WorkspaceRootError::Permanent(format!(
                "Failed to inspect workspace root {}: {error}",
                root.display()
            ))
        })?;
        if !metadata.is_dir() {
            return Err(WorkspaceRootError::Permanent(format!(
                "Workspace root is not a directory: {}",
                root.display()
            )));
        }
        Ok(Self {
            root,
            requested_root,
            directory,
        })
    }

    #[cfg(target_os = "linux")]
    fn check_platform_support(directory: &File) -> Result<(), WorkspaceRootError> {
        openat2(directory, ".", PIN_FLAGS, Mode::empty(), RESOLVE_FLAGS)
            .map(drop)
            .map_err(platform_support_error)
    }

    /// Returns the display identity derived from the pinned root descriptor.
    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub fn resolve_user_path(&self, cwd: &Path, path: &str) -> Result<PathBuf, String> {
        let expanded = expand_tilde(path);
        let absolute = if expanded.is_absolute() {
            expanded
        } else {
            cwd.join(expanded)
        };
        let relative = self.strip_root_prefix(&absolute).map_err(|_| {
            format!(
                "Path escapes workspace root {}: {}",
                self.root.display(),
                absolute.display()
            )
        })?;
        if relative.as_os_str().is_empty() {
            Ok(PathBuf::from("."))
        } else {
            Ok(relative.to_path_buf())
        }
    }

    pub fn display_path(&self, relative_path: &Path) -> PathBuf {
        self.root.join(relative_path)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn prepare_write(
        &self,
        cwd: &Path,
        path: &str,
        create_parents: bool,
    ) -> Result<WorkspaceWriteTarget, String> {
        let must_be_directory = path_requires_directory(Path::new(path));
        let relative_path = self.resolve_user_path(cwd, path)?;
        match self.resolve_beneath(
            &relative_path,
            ResolveMode::Write {
                create_parents,
                must_be_directory,
            },
        ) {
            Ok(ResolvedBeneath::Write(target)) => Ok(target),
            Ok(ResolvedBeneath::Node(_)) => Err(format!(
                "Path does not name a writable file: {}",
                self.display_path(&relative_path).display()
            )),
            Err(error) => Err(error.to_string()),
        }
    }

    #[cfg(target_os = "macos")]
    pub(super) fn prepare_write(
        &self,
        cwd: &Path,
        path: &str,
        create_parents: bool,
    ) -> Result<WorkspaceWriteTarget, String> {
        let must_be_directory = path_requires_directory(Path::new(path));
        let relative_path = self.resolve_user_path(cwd, path)?;
        self.prepare_write_beneath(&relative_path, create_parents, must_be_directory)
            .map_err(|error| error.to_string())
    }

    #[cfg(target_os = "macos")]
    fn prepare_write_beneath(
        &self,
        relative_path: &Path,
        create_parents: bool,
        must_be_directory: bool,
    ) -> Result<WorkspaceWriteTarget, WorkspaceOpenError> {
        let display_path = self.display_path(relative_path);
        let mut remaining = RemainingPath::new(relative_path, must_be_directory)?;
        let root = self.directory.try_clone().map_err(|error| {
            WorkspaceOpenError::Other(format!(
                "Failed to clone workspace root descriptor {}: {error}",
                self.root.display()
            ))
        })?;
        let mut directories = vec![root];
        let root_device = directories[0]
            .metadata()
            .map_err(|error| {
                WorkspaceOpenError::Other(format!(
                    "Failed to inspect pinned workspace root {}: {error}",
                    self.root.display()
                ))
            })?
            .dev();
        let mut followed_symlinks = 0;

        loop {
            let Some(component) = remaining.pop_front() else {
                return Err(WorkspaceOpenError::Other(format!(
                    "Path must name a file: {}",
                    display_path.display()
                )));
            };
            if component == "." {
                continue;
            }
            if component == ".." {
                if directories.len() == 1 {
                    return Err(WorkspaceOpenError::Escape(display_path));
                }
                directories.pop();
                continue;
            }

            let current = directories.last().expect("workspace root descriptor");
            let metadata = match statat(current, &component, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(metadata) => metadata,
                Err(rustix::io::Errno::NOENT) if remaining.is_empty() => {
                    if remaining.must_be_directory() {
                        return Err(WorkspaceOpenError::Other(format!(
                            "Not a directory: {}",
                            display_path.display()
                        )));
                    }
                    return Ok(WorkspaceWriteTarget {
                        parent: current.try_clone().map_err(|error| {
                            WorkspaceOpenError::Other(format!(
                                "Failed to clone parent directory for {}: {error}",
                                display_path.display()
                            ))
                        })?,
                        name: component,
                        display_path,
                    });
                }
                Err(rustix::io::Errno::NOENT) if create_parents => {
                    match mkdirat(current, &component, Mode::from_raw_mode(0o777)) {
                        Ok(()) | Err(rustix::io::Errno::EXIST) => {
                            remaining.push_front(component);
                            continue;
                        }
                        Err(error) => {
                            return Err(WorkspaceOpenError::Other(format!(
                                "Failed to create parent directory {}: {error}",
                                display_path.display()
                            )));
                        }
                    }
                }
                Err(rustix::io::Errno::NOENT) => {
                    return Err(WorkspaceOpenError::Other(format!(
                        "Path not found: {}",
                        display_path.display()
                    )));
                }
                Err(rustix::io::Errno::ACCESS | rustix::io::Errno::PERM) => {
                    return Err(WorkspaceOpenError::Inaccessible(display_path));
                }
                Err(error) => {
                    return Err(WorkspaceOpenError::Other(format!(
                        "Failed to inspect {}: {error}",
                        display_path.display()
                    )));
                }
            };
            let file_type = FileType::from_raw_mode(metadata.st_mode);

            if file_type == FileType::Symlink {
                followed_symlinks += 1;
                if followed_symlinks > MAX_SYMLINKS {
                    return Err(WorkspaceOpenError::SymlinkLoop(display_path));
                }
                let target = readlinkat(current, &component, Vec::new()).map_err(|error| {
                    WorkspaceOpenError::Other(format!(
                        "Failed to read symbolic link {}: {error}",
                        display_path.display()
                    ))
                })?;
                let verified = statat(current, &component, AtFlags::SYMLINK_NOFOLLOW)
                    .map_err(|_| WorkspaceOpenError::Escape(display_path.clone()))?;
                if FileType::from_raw_mode(verified.st_mode) != FileType::Symlink
                    || metadata.st_dev != verified.st_dev
                    || metadata.st_ino != verified.st_ino
                {
                    return Err(WorkspaceOpenError::Escape(display_path));
                }

                let target = PathBuf::from(OsString::from_vec(target.into_bytes()));
                let target_must_be_directory = path_requires_directory(&target);
                let target = if target.is_absolute() {
                    let target = self
                        .strip_root_prefix(&target)
                        .map_err(|_| WorkspaceOpenError::Escape(display_path.clone()))?;
                    directories.truncate(1);
                    target
                } else {
                    target.as_path()
                };
                remaining.prepend(target, target_must_be_directory)?;
                continue;
            }

            if metadata.st_dev as u64 != root_device {
                return Err(WorkspaceOpenError::Escape(display_path));
            }

            if remaining.is_empty() {
                if remaining.must_be_directory() && file_type != FileType::Directory {
                    return Err(WorkspaceOpenError::Other(format!(
                        "Not a directory: {}",
                        display_path.display()
                    )));
                }
                return if file_type == FileType::RegularFile {
                    Ok(WorkspaceWriteTarget {
                        parent: current.try_clone().map_err(|error| {
                            WorkspaceOpenError::Other(format!(
                                "Failed to clone parent directory for {}: {error}",
                                display_path.display()
                            ))
                        })?,
                        name: component,
                        display_path,
                    })
                } else if file_type == FileType::Directory {
                    Err(WorkspaceOpenError::Other(format!(
                        "Not a file: {}",
                        display_path.display()
                    )))
                } else {
                    Err(WorkspaceOpenError::Unsupported(display_path))
                };
            }
            if file_type != FileType::Directory {
                return Err(WorkspaceOpenError::Other(format!(
                    "Not a directory: {}",
                    display_path.display()
                )));
            }

            let flags = OFlags::RDONLY
                | OFlags::DIRECTORY
                | OFlags::CLOEXEC
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK;
            let directory = match openat(current, &component, flags, Mode::empty()) {
                Ok(directory) => directory,
                Err(rustix::io::Errno::LOOP) => {
                    return Err(WorkspaceOpenError::Escape(display_path));
                }
                Err(error) => {
                    return Err(WorkspaceOpenError::Other(format!(
                        "Failed to open parent directory {}: {error}",
                        display_path.display()
                    )));
                }
            };
            let directory = File::from(directory);
            let opened = directory.metadata().map_err(|error| {
                WorkspaceOpenError::Other(format!(
                    "Failed to inspect parent directory {}: {error}",
                    display_path.display()
                ))
            })?;
            if opened.dev() != root_device
                || opened.dev() != metadata.st_dev as u64
                || opened.ino() != metadata.st_ino as u64
            {
                return Err(WorkspaceOpenError::Escape(display_path));
            }
            directories.push(directory);
        }
    }

    fn strip_root_prefix<'a>(&self, path: &'a Path) -> Result<&'a Path, ()> {
        path.strip_prefix(&self.root)
            .or_else(|_| path.strip_prefix(&self.requested_root))
            .map_err(|_| ())
    }

    pub fn open_node(&self, cwd: &Path, path: &str) -> Result<WorkspaceNode, String> {
        let relative_path = self.resolve_user_path(cwd, path)?;
        self.open_relative_node(&relative_path)?.ok_or_else(|| {
            format!(
                "Path not found: {}",
                self.display_path(&relative_path).display()
            )
        })
    }

    pub fn try_open_node(&self, cwd: &Path, path: &str) -> Result<Option<WorkspaceNode>, String> {
        let relative_path = self.resolve_user_path(cwd, path)?;
        self.open_relative_node(&relative_path)
    }

    pub fn try_open_batch_node(
        &self,
        cwd: &Path,
        path: &str,
    ) -> Result<Option<WorkspaceBatchNode>, String> {
        let relative_path = self.resolve_user_path(cwd, path)?;
        match self.open_relative_node_inner(&relative_path) {
            Ok(node) => Ok(node.map(WorkspaceBatchNode::Node)),
            Err(WorkspaceOpenError::Inaccessible(display_path)) => {
                match self.pin_relative_node_kind(&relative_path) {
                    Ok(Some(WorkspaceNodeKind::File)) => {
                        Ok(Some(WorkspaceBatchNode::InaccessibleFile {
                            display_path,
                            error: "Permission denied".to_string(),
                        }))
                    }
                    Ok(Some(WorkspaceNodeKind::Directory)) => {
                        Err(WorkspaceOpenError::Inaccessible(display_path).to_string())
                    }
                    Ok(None) => Ok(None),
                    Err(error) => Err(error.to_string()),
                }
            }
            Err(error) => Err(error.to_string()),
        }
    }

    pub fn try_open_path_node(
        &self,
        cwd: &Path,
        path: &str,
    ) -> Result<Option<WorkspacePathNode>, String> {
        let relative_path = self.resolve_user_path(cwd, path)?;
        let kind = self
            .pin_relative_node_kind(&relative_path)
            .map_err(|error| error.to_string())?;
        match kind {
            Some(WorkspaceNodeKind::File) => Ok(Some(WorkspacePathNode::File(
                self.display_path(&relative_path),
            ))),
            Some(WorkspaceNodeKind::Directory) => {
                match self.open_relative_node_inner(&relative_path) {
                    Ok(Some(WorkspaceNode::Directory(directory))) => {
                        Ok(Some(WorkspacePathNode::Directory(directory)))
                    }
                    Ok(Some(WorkspaceNode::File(_))) | Ok(None) => Ok(None),
                    Err(WorkspaceOpenError::Inaccessible(display_path)) => {
                        Ok(Some(WorkspacePathNode::InaccessibleDirectory {
                            display_path,
                            error: "Permission denied".to_string(),
                        }))
                    }
                    Err(error) => Err(error.to_string()),
                }
            }
            None => Ok(None),
        }
    }

    pub fn open_file(&self, cwd: &Path, path: &str) -> Result<WorkspaceFile, String> {
        match self.open_node(cwd, path)? {
            WorkspaceNode::File(file) => Ok(file),
            WorkspaceNode::Directory(directory) => {
                Err(format!("Not a file: {}", directory.display_path.display()))
            }
        }
    }

    /// Reopens a previously discovered display path beneath the pinned root.
    pub fn open_display_file(&self, path: &Path) -> Result<WorkspaceFile, String> {
        let relative_path = self.strip_root_prefix(path).map_err(|_| {
            format!(
                "Path escapes workspace root {}: {}",
                self.root.display(),
                path.display()
            )
        })?;
        match self.open_relative_node(relative_path)? {
            Some(WorkspaceNode::File(file)) => Ok(file),
            Some(WorkspaceNode::Directory(directory)) => {
                Err(format!("Not a file: {}", directory.display_path.display()))
            }
            None => Err(format!("Path not found: {}", path.display())),
        }
    }

    pub fn open_directory(&self, cwd: &Path, path: &str) -> Result<WorkspaceDirectory, String> {
        match self.open_node(cwd, path)? {
            WorkspaceNode::Directory(directory) => Ok(directory),
            WorkspaceNode::File(file) => {
                Err(format!("Not a directory: {}", file.display_path.display()))
            }
        }
    }

    pub fn open_relative_node(
        &self,
        relative_path: &Path,
    ) -> Result<Option<WorkspaceNode>, String> {
        self.open_relative_node_inner(relative_path)
            .map_err(|error| error.to_string())
    }

    fn open_relative_node_inner(
        &self,
        relative_path: &Path,
    ) -> Result<Option<WorkspaceNode>, WorkspaceOpenError> {
        if relative_path.is_absolute() {
            return Err(WorkspaceOpenError::Other(format!(
                "Workspace-relative path must not be absolute: {}",
                relative_path.display()
            )));
        }
        let Some(file) = self.open_beneath(relative_path)? else {
            return Ok(None);
        };
        let metadata = file.metadata().map_err(|error| {
            WorkspaceOpenError::Other(format!(
                "Failed to inspect {}: {error}",
                self.display_path(relative_path).display()
            ))
        })?;
        let display_path = self.display_path(relative_path);
        if metadata.is_file() {
            Ok(Some(WorkspaceNode::File(WorkspaceFile {
                relative_path: relative_path.to_path_buf(),
                display_path,
                file,
            })))
        } else if metadata.is_dir() {
            Ok(Some(WorkspaceNode::Directory(WorkspaceDirectory {
                relative_path: relative_path.to_path_buf(),
                display_path,
                file,
            })))
        } else {
            Err(WorkspaceOpenError::Unsupported(display_path))
        }
    }

    #[cfg(target_os = "linux")]
    fn open_beneath(&self, relative_path: &Path) -> Result<Option<File>, WorkspaceOpenError> {
        match self.resolve_beneath(relative_path, ResolveMode::Readable)? {
            ResolvedBeneath::Node(node) => Ok(node),
            ResolvedBeneath::Write(_) => unreachable!("read resolution returned a write target"),
        }
    }

    #[cfg(target_os = "macos")]
    fn open_beneath(&self, relative_path: &Path) -> Result<Option<File>, WorkspaceOpenError> {
        self.resolve_beneath(relative_path)
    }

    #[cfg(target_os = "linux")]
    fn pin_beneath(&self, relative_path: &Path) -> Result<Option<File>, WorkspaceOpenError> {
        match self.resolve_beneath(relative_path, ResolveMode::Pinned)? {
            ResolvedBeneath::Node(node) => Ok(node),
            ResolvedBeneath::Write(_) => unreachable!("pin resolution returned a write target"),
        }
    }

    #[cfg(target_os = "linux")]
    fn resolve_beneath(
        &self,
        relative_path: &Path,
        mode: ResolveMode,
    ) -> Result<ResolvedBeneath, WorkspaceOpenError> {
        let display_path = self.display_path(relative_path);
        let must_be_directory = match mode {
            ResolveMode::Write {
                must_be_directory, ..
            } => must_be_directory,
            ResolveMode::Readable | ResolveMode::Pinned => path_requires_directory(relative_path),
        };
        let mut remaining = RemainingPath::new(relative_path, must_be_directory)?;
        let root = self.directory.try_clone().map_err(|error| {
            WorkspaceOpenError::Other(format!(
                "Failed to clone workspace root descriptor {}: {error}",
                self.root.display()
            ))
        })?;
        let mut directories = vec![root];
        let mut followed_symlinks = 0;

        loop {
            let Some(component) = remaining.pop_front() else {
                return match mode {
                    ResolveMode::Readable => Ok(ResolvedBeneath::Node(reopen_pinned(
                        directories.last().expect("workspace root descriptor"),
                        &display_path,
                        true,
                    )?)),
                    ResolveMode::Pinned => Ok(ResolvedBeneath::Node(directories.pop())),
                    ResolveMode::Write { .. } => Err(WorkspaceOpenError::Other(format!(
                        "Path must name a file: {}",
                        display_path.display()
                    ))),
                };
            };
            if component == "." {
                continue;
            }
            if component == ".." {
                if directories.len() == 1 {
                    return Err(WorkspaceOpenError::Escape(display_path));
                }
                directories.pop();
                continue;
            }

            let current = directories.last().expect("workspace root descriptor");
            // Pin before inspecting so a concurrent rename cannot change which
            // symlink target or directory the remainder is resolved against.
            let pinned =
                match open_component(current, Path::new(&component), &display_path, PIN_FLAGS)? {
                    Some(pinned) => pinned,
                    None => {
                        if remaining.is_empty() && matches!(mode, ResolveMode::Write { .. }) {
                            if remaining.must_be_directory() {
                                return Err(WorkspaceOpenError::Other(format!(
                                    "Not a directory: {}",
                                    display_path.display()
                                )));
                            }
                            return Ok(ResolvedBeneath::Write(WorkspaceWriteTarget {
                                parent: current.try_clone().map_err(|error| {
                                    WorkspaceOpenError::Other(format!(
                                        "Failed to clone parent directory for {}: {error}",
                                        display_path.display()
                                    ))
                                })?,
                                name: component,
                                display_path,
                            }));
                        }
                        if !matches!(
                            mode,
                            ResolveMode::Write {
                                create_parents: true,
                                ..
                            }
                        ) {
                            return match mode {
                                ResolveMode::Readable | ResolveMode::Pinned => {
                                    Ok(ResolvedBeneath::Node(None))
                                }
                                ResolveMode::Write { .. } => Err(WorkspaceOpenError::Other(
                                    format!("Path not found: {}", display_path.display()),
                                )),
                            };
                        }
                        match mkdirat(current, &component, Mode::from_raw_mode(0o777)) {
                            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                            Err(error) => {
                                return Err(WorkspaceOpenError::Other(format!(
                                    "Failed to create parent directory {}: {error}",
                                    display_path.display()
                                )))
                            }
                        }
                        open_component(current, Path::new(&component), &display_path, PIN_FLAGS)?
                            .ok_or_else(|| {
                                WorkspaceOpenError::Other(format!(
                                    "Parent directory disappeared while opening {}",
                                    display_path.display()
                                ))
                            })?
                    }
                };
            let metadata = pinned.metadata().map_err(|error| {
                WorkspaceOpenError::Other(format!(
                    "Failed to inspect {}: {error}",
                    display_path.display()
                ))
            })?;

            if metadata.file_type().is_symlink() {
                followed_symlinks += 1;
                if followed_symlinks > MAX_SYMLINKS {
                    return Err(WorkspaceOpenError::SymlinkLoop(display_path));
                }
                let target = readlinkat(&pinned, "", Vec::new()).map_err(|error| {
                    WorkspaceOpenError::Other(format!(
                        "Failed to read symbolic link {}: {error}",
                        display_path.display()
                    ))
                })?;
                let target = PathBuf::from(OsString::from_vec(target.into_bytes()));
                let target_must_be_directory = path_requires_directory(&target);
                let target = if target.is_absolute() {
                    // The kernel cannot reinterpret a host-absolute target for
                    // RESOLVE_BENEATH, so reroot only lexically internal targets.
                    let target = self
                        .strip_root_prefix(&target)
                        .map_err(|_| WorkspaceOpenError::Escape(display_path.clone()))?;
                    directories.truncate(1);
                    target
                } else {
                    target.as_path()
                };
                remaining.prepend(target, target_must_be_directory)?;
                continue;
            }

            if !remaining.is_empty() {
                if !metadata.is_dir() {
                    return Err(WorkspaceOpenError::Other(format!(
                        "Not a directory: {}",
                        display_path.display()
                    )));
                }
                directories.push(pinned);
                continue;
            }

            if remaining.must_be_directory() && !metadata.is_dir() {
                return Err(WorkspaceOpenError::Other(format!(
                    "Not a directory: {}",
                    display_path.display()
                )));
            }

            return match mode {
                ResolveMode::Readable if metadata.is_file() => Ok(ResolvedBeneath::Node(
                    reopen_pinned(&pinned, &display_path, false)?,
                )),
                ResolveMode::Readable if metadata.is_dir() => Ok(ResolvedBeneath::Node(
                    reopen_pinned(&pinned, &display_path, true)?,
                )),
                ResolveMode::Readable => Err(WorkspaceOpenError::Unsupported(display_path)),
                ResolveMode::Pinned => Ok(ResolvedBeneath::Node(Some(pinned))),
                ResolveMode::Write { .. } if metadata.is_file() => {
                    Ok(ResolvedBeneath::Write(WorkspaceWriteTarget {
                        parent: current.try_clone().map_err(|error| {
                            WorkspaceOpenError::Other(format!(
                                "Failed to clone parent directory for {}: {error}",
                                display_path.display()
                            ))
                        })?,
                        name: component,
                        display_path,
                    }))
                }
                ResolveMode::Write { .. } if metadata.is_dir() => Err(WorkspaceOpenError::Other(
                    format!("Not a file: {}", display_path.display()),
                )),
                ResolveMode::Write { .. } => Err(WorkspaceOpenError::Unsupported(display_path)),
            };
        }
    }

    #[cfg(target_os = "macos")]
    fn resolve_beneath(&self, relative_path: &Path) -> Result<Option<File>, WorkspaceOpenError> {
        // macOS has no openat2 equivalent. Canonical paths plus descriptor
        // identity checks provide best-effort confinement around each open.
        self.verify_pinned_root()?;
        let display_path = self.display_path(relative_path);
        let candidate = self.root.join(relative_path);
        let canonical = match candidate.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(WorkspaceOpenError::Inaccessible(display_path));
            }
            Err(error)
                if rustix::io::Errno::from_io_error(&error) == Some(rustix::io::Errno::LOOP) =>
            {
                return Err(WorkspaceOpenError::SymlinkLoop(display_path));
            }
            Err(error) => {
                return Err(WorkspaceOpenError::Other(format!(
                    "Failed to resolve {}: {error}",
                    display_path.display()
                )));
            }
        };
        if canonical.strip_prefix(&self.root).is_err() {
            return Err(WorkspaceOpenError::Escape(display_path));
        }

        let root_metadata = self.directory.metadata().map_err(|error| {
            WorkspaceOpenError::Other(format!(
                "Failed to inspect pinned workspace root {}: {error}",
                self.root.display()
            ))
        })?;
        let resolved_metadata = std::fs::metadata(&canonical).map_err(|error| {
            WorkspaceOpenError::Other(format!(
                "Failed to inspect {}: {error}",
                display_path.display()
            ))
        })?;
        if resolved_metadata.dev() != root_metadata.dev() {
            return Err(WorkspaceOpenError::Escape(display_path));
        }
        // macOS has no O_PATH equivalent, so metadata-only callers may still
        // require read permission when pinning an object.
        let flags = if resolved_metadata.is_dir() {
            READ_FLAGS | OFlags::DIRECTORY | OFlags::NOFOLLOW
        } else if resolved_metadata.is_file() {
            READ_FLAGS | OFlags::NOFOLLOW
        } else {
            return Err(WorkspaceOpenError::Unsupported(display_path));
        };
        let descriptor = open(&canonical, flags, Mode::empty()).map_err(|error| match error {
            rustix::io::Errno::ACCESS | rustix::io::Errno::PERM => {
                WorkspaceOpenError::Inaccessible(display_path.clone())
            }
            rustix::io::Errno::LOOP => WorkspaceOpenError::Escape(display_path.clone()),
            _ => WorkspaceOpenError::Other(format!(
                "Failed to open {}: {error}",
                display_path.display()
            )),
        })?;
        let file = File::from(descriptor);
        let opened_metadata = file.metadata().map_err(|error| {
            WorkspaceOpenError::Other(format!(
                "Failed to inspect {}: {error}",
                display_path.display()
            ))
        })?;

        let verified = candidate.canonicalize().map_err(|error| {
            WorkspaceOpenError::Other(format!(
                "Failed to verify {} after opening: {error}",
                display_path.display()
            ))
        })?;
        let verified_metadata = std::fs::metadata(&verified).map_err(|error| {
            WorkspaceOpenError::Other(format!(
                "Failed to inspect {} after opening: {error}",
                display_path.display()
            ))
        })?;
        if verified.strip_prefix(&self.root).is_err()
            || verified_metadata.dev() != root_metadata.dev()
            || opened_metadata.dev() != verified_metadata.dev()
            || opened_metadata.ino() != verified_metadata.ino()
        {
            return Err(WorkspaceOpenError::Escape(display_path));
        }
        self.verify_pinned_root()?;
        Ok(Some(file))
    }

    #[cfg(target_os = "macos")]
    fn metadata_beneath(
        &self,
        relative_path: &Path,
    ) -> Result<Option<std::fs::Metadata>, WorkspaceOpenError> {
        // Metadata-only consumers reopen paths before reading or traversing, so
        // repeated identity checks can preserve confinement without read access.
        self.verify_pinned_root()?;
        let display_path = self.display_path(relative_path);
        let candidate = self.root.join(relative_path);
        let canonical = match candidate.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(WorkspaceOpenError::Inaccessible(display_path));
            }
            Err(error)
                if rustix::io::Errno::from_io_error(&error) == Some(rustix::io::Errno::LOOP) =>
            {
                return Err(WorkspaceOpenError::SymlinkLoop(display_path));
            }
            Err(error) => {
                return Err(WorkspaceOpenError::Other(format!(
                    "Failed to resolve {}: {error}",
                    display_path.display()
                )));
            }
        };
        if canonical.strip_prefix(&self.root).is_err() {
            return Err(WorkspaceOpenError::Escape(display_path));
        }

        let root_metadata = self.directory.metadata().map_err(|error| {
            WorkspaceOpenError::Other(format!(
                "Failed to inspect pinned workspace root {}: {error}",
                self.root.display()
            ))
        })?;
        let resolved_metadata = std::fs::metadata(&canonical).map_err(|error| {
            WorkspaceOpenError::Other(format!(
                "Failed to inspect {}: {error}",
                display_path.display()
            ))
        })?;
        if resolved_metadata.dev() != root_metadata.dev() {
            return Err(WorkspaceOpenError::Escape(display_path));
        }

        let verified = candidate.canonicalize().map_err(|error| {
            WorkspaceOpenError::Other(format!(
                "Failed to verify {} after inspection: {error}",
                display_path.display()
            ))
        })?;
        let verified_metadata = std::fs::metadata(&verified).map_err(|error| {
            WorkspaceOpenError::Other(format!(
                "Failed to inspect {} after verification: {error}",
                display_path.display()
            ))
        })?;
        if verified.strip_prefix(&self.root).is_err()
            || verified_metadata.dev() != root_metadata.dev()
            || resolved_metadata.dev() != verified_metadata.dev()
            || resolved_metadata.ino() != verified_metadata.ino()
        {
            return Err(WorkspaceOpenError::Escape(display_path));
        }
        self.verify_pinned_root()?;
        Ok(Some(verified_metadata))
    }

    #[cfg(target_os = "macos")]
    fn verify_pinned_root(&self) -> Result<(), WorkspaceOpenError> {
        let pinned = self.directory.metadata().map_err(|error| {
            WorkspaceOpenError::Other(format!(
                "Failed to inspect pinned workspace root {}: {error}",
                self.root.display()
            ))
        })?;
        let live = std::fs::metadata(&self.root).map_err(|error| {
            WorkspaceOpenError::Other(format!(
                "Pinned workspace root changed: {}: {error}",
                self.root.display()
            ))
        })?;
        if !live.is_dir() || pinned.dev() != live.dev() || pinned.ino() != live.ino() {
            return Err(WorkspaceOpenError::Other(format!(
                "Pinned workspace root was replaced: {}",
                self.root.display()
            )));
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn pin_relative_node_kind(
        &self,
        relative_path: &Path,
    ) -> Result<Option<WorkspaceNodeKind>, WorkspaceOpenError> {
        let Some(file) = self.pin_beneath(relative_path)? else {
            return Ok(None);
        };
        let metadata = file.metadata().map_err(|error| {
            WorkspaceOpenError::Other(format!(
                "Failed to inspect {}: {error}",
                self.display_path(relative_path).display()
            ))
        })?;
        if metadata.is_file() {
            Ok(Some(WorkspaceNodeKind::File))
        } else if metadata.is_dir() {
            Ok(Some(WorkspaceNodeKind::Directory))
        } else {
            Err(WorkspaceOpenError::Unsupported(
                self.display_path(relative_path),
            ))
        }
    }

    #[cfg(target_os = "macos")]
    fn pin_relative_node_kind(
        &self,
        relative_path: &Path,
    ) -> Result<Option<WorkspaceNodeKind>, WorkspaceOpenError> {
        let Some(metadata) = self.metadata_beneath(relative_path)? else {
            return Ok(None);
        };
        if metadata.is_file() {
            Ok(Some(WorkspaceNodeKind::File))
        } else if metadata.is_dir() {
            Ok(Some(WorkspaceNodeKind::Directory))
        } else {
            Err(WorkspaceOpenError::Unsupported(
                self.display_path(relative_path),
            ))
        }
    }

    pub fn is_relative_directory(&self, relative_path: &Path) -> bool {
        matches!(
            self.pin_relative_node_kind(relative_path),
            Ok(Some(WorkspaceNodeKind::Directory))
        )
    }

    fn classify_walk_entry(
        &self,
        relative_path: &Path,
        entry_type: FileType,
    ) -> Result<Option<WorkspaceNodeKind>, WorkspaceOpenError> {
        match entry_type {
            FileType::RegularFile => Ok(Some(WorkspaceNodeKind::File)),
            FileType::Directory => Ok(Some(WorkspaceNodeKind::Directory)),
            FileType::Symlink | FileType::Unknown => self.pin_relative_node_kind(relative_path),
            _ => Ok(None),
        }
    }

    pub fn walk_files<F>(
        &self,
        directory: WorkspaceDirectory,
        max_files: usize,
        include: F,
    ) -> Result<WalkedFiles, String>
    where
        F: FnMut(&Path) -> bool,
    {
        self.walk_files_with_ignores(directory, max_files, false, include)
    }

    fn walk_files_with_ignores<F>(
        &self,
        directory: WorkspaceDirectory,
        max_files: usize,
        respect_ignores: bool,
        include: F,
    ) -> Result<WalkedFiles, String>
    where
        F: FnMut(&Path) -> bool,
    {
        let (mut files, truncated, _) = self.walk_matching_files(
            directory,
            max_files,
            respect_ignores,
            include,
            |workspace, relative_path| match workspace.open_relative_node_inner(relative_path)? {
                Some(WorkspaceNode::File(file)) => Ok(Some(file)),
                Some(WorkspaceNode::Directory(_)) | None => Ok(None),
            },
        )?;
        files.sort_by(|left, right| left.display_path.cmp(&right.display_path));
        Ok(WalkedFiles { files, truncated })
    }

    pub fn walk_file_paths<F>(
        &self,
        directory: WorkspaceDirectory,
        max_files: usize,
        include: F,
    ) -> Result<WalkedFilePaths, String>
    where
        F: FnMut(&Path) -> bool,
    {
        let (mut paths, truncated, _) = self.walk_matching_files(
            directory,
            max_files,
            false,
            include,
            |workspace, relative_path| match workspace.pin_relative_node_kind(relative_path)? {
                Some(WorkspaceNodeKind::File) => Ok(Some(workspace.display_path(relative_path))),
                Some(WorkspaceNodeKind::Directory) | None => Ok(None),
            },
        )?;
        paths.sort();
        Ok(WalkedFilePaths { paths, truncated })
    }

    pub fn walk_relative_file_paths<F>(
        &self,
        directory: WorkspaceDirectory,
        max_files: usize,
        respect_ignores: bool,
        mut include: F,
    ) -> Result<WalkedRelativeFilePaths, String>
    where
        F: FnMut(&Path) -> bool,
    {
        let base_path = directory.relative_path.clone();
        let (mut paths, truncated, ignore_incomplete) = self.walk_matching_files(
            directory,
            max_files,
            respect_ignores,
            |relative_to_base| include(&base_path.join(relative_to_base)),
            |workspace, relative_path| match workspace.pin_relative_node_kind(relative_path)? {
                Some(WorkspaceNodeKind::File) => Ok(Some(relative_path.to_path_buf())),
                Some(WorkspaceNodeKind::Directory) | None => Ok(None),
            },
        )?;
        paths.sort();
        Ok(WalkedRelativeFilePaths {
            paths,
            truncated,
            ignore_incomplete,
        })
    }

    fn walk_matching_files<F, T, C>(
        &self,
        directory: WorkspaceDirectory,
        max_files: usize,
        respect_ignores: bool,
        mut include: F,
        mut collect_file: C,
    ) -> Result<(Vec<T>, bool, bool), String>
    where
        F: FnMut(&Path) -> bool,
        C: FnMut(&Self, &Path) -> Result<Option<T>, WorkspaceOpenError>,
    {
        let base_path = directory.relative_path.clone();
        let ignore_base_path = if respect_ignores {
            normalize_workspace_relative_path(&base_path)
        } else {
            PathBuf::new()
        };
        let mut ignore_budget = MAX_IGNORE_TOTAL_BYTES;
        let (initial_ignore_matchers, initial_inside_git, initial_ignore_incomplete) =
            if respect_ignores {
                self.load_parent_ignore_matchers(&ignore_base_path, &mut ignore_budget)?
            } else {
                (IgnoreMatchers::default(), false, false)
            };
        let mut queue = VecDeque::from([(
            PathBuf::new(),
            Some(directory.file),
            Vec::new(),
            initial_ignore_matchers,
            initial_inside_git,
        )]);
        let mut files = Vec::new();
        let mut visited_entries = 0;
        let mut incomplete = false;
        let mut ignore_incomplete = initial_ignore_incomplete;
        let mut limit_reached = false;

        while let Some((
            relative_directory,
            pinned_directory,
            mut ancestors,
            mut ignore_matchers,
            mut inside_git,
        )) = queue.pop_front()
        {
            let directory_file = match pinned_directory {
                Some(file) => file,
                None => {
                    let relative_to_root = base_path.join(&relative_directory);
                    match self.open_relative_node_inner(&relative_to_root) {
                        Ok(Some(WorkspaceNode::Directory(directory))) => directory.file,
                        Ok(Some(WorkspaceNode::File(_))) | Ok(None) => continue,
                        Err(WorkspaceOpenError::Escape(_))
                        | Err(WorkspaceOpenError::Unsupported(_)) => continue,
                        Err(WorkspaceOpenError::Inaccessible(_))
                        | Err(WorkspaceOpenError::SymlinkLoop(_)) => {
                            incomplete = true;
                            continue;
                        }
                        Err(error) => return Err(error.to_string()),
                    }
                }
            };
            let metadata = directory_file.metadata().map_err(|error| {
                format!(
                    "Failed to inspect directory {}: {error}",
                    self.display_path(&base_path.join(&relative_directory))
                        .display()
                )
            })?;
            let identity = (metadata.dev(), metadata.ino());
            if ancestors.contains(&identity) {
                continue;
            }
            ancestors.push(identity);
            if respect_ignores {
                let relative_to_root = base_path.join(&relative_directory);
                let ignore_relative_to_root = ignore_base_path.join(&relative_directory);
                let git_directory = match self.git_marker(&relative_to_root) {
                    Ok(Some(marker)) => {
                        ignore_matchers.git_exclude.clear();
                        ignore_matchers.gitignore.clear();
                        inside_git = true;
                        marker.is_directory
                    }
                    Ok(None) => false,
                    Err(WorkspaceOpenError::Inaccessible(_))
                    | Err(WorkspaceOpenError::Escape(_))
                    | Err(WorkspaceOpenError::SymlinkLoop(_)) => {
                        ignore_incomplete = true;
                        false
                    }
                    Err(WorkspaceOpenError::Unsupported(_)) => false,
                    Err(error) => return Err(error.to_string()),
                };
                match self.load_ignore_matchers(
                    &relative_to_root,
                    &ignore_relative_to_root,
                    inside_git,
                    git_directory,
                    &mut ignore_budget,
                ) {
                    Ok(loaded) => {
                        ignore_incomplete |= loaded.incomplete;
                        ignore_matchers.extend(loaded);
                    }
                    Err(WorkspaceOpenError::Inaccessible(_))
                    | Err(WorkspaceOpenError::Escape(_))
                    | Err(WorkspaceOpenError::SymlinkLoop(_)) => ignore_incomplete = true,
                    Err(WorkspaceOpenError::Unsupported(_)) => {}
                    Err(error) => return Err(error.to_string()),
                }
            }
            let reader = match Dir::read_from(&directory_file) {
                Ok(entries) => entries,
                Err(error) if is_permission_error(error) => {
                    incomplete = true;
                    continue;
                }
                Err(error) => {
                    return Err(format!(
                        "Failed to list directory {}: {error}",
                        self.display_path(&base_path.join(&relative_directory))
                            .display()
                    ));
                }
            };
            let remaining_entries = MAX_WALK_ENTRIES.saturating_sub(visited_entries);
            let mut entries = Vec::new();
            let mut entries_overflow = false;
            for entry in reader {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) if is_permission_error(error) => {
                        incomplete = true;
                        break;
                    }
                    Err(error) => {
                        return Err(format!(
                            "Failed to list directory {}: {error}",
                            self.display_path(&base_path.join(&relative_directory))
                                .display()
                        ));
                    }
                };
                let name_bytes = entry.file_name().to_bytes();
                if name_bytes == b"." || name_bytes == b".." {
                    continue;
                }
                if entries.len() >= remaining_entries {
                    entries_overflow = true;
                    break;
                }
                let name = OsString::from_vec(name_bytes.to_vec());
                entries.push((name, entry.file_type(), name_bytes.first() == Some(&b'.')));
            }
            if entries_overflow {
                limit_reached = true;
                break;
            }
            visited_entries += entries.len();
            entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));

            for (name, entry_type, hidden) in entries {
                let relative_to_base = relative_directory.join(name);
                let relative_to_root = base_path.join(&relative_to_base);
                let node_kind = match self.classify_walk_entry(&relative_to_root, entry_type) {
                    Ok(Some(node_kind)) => node_kind,
                    Ok(None)
                    | Err(WorkspaceOpenError::Escape(_))
                    | Err(WorkspaceOpenError::Unsupported(_)) => continue,
                    Err(WorkspaceOpenError::Inaccessible(_))
                    | Err(WorkspaceOpenError::SymlinkLoop(_)) => {
                        incomplete = true;
                        continue;
                    }
                    Err(error) => return Err(error.to_string()),
                };
                if respect_ignores {
                    let ignore_relative_to_root = ignore_base_path.join(&relative_to_base);
                    let ignore_status = ignore_status(
                        &ignore_matchers,
                        &ignore_relative_to_root,
                        node_kind == WorkspaceNodeKind::Directory,
                    );
                    if ignore_status == Some(true) || (hidden && ignore_status != Some(false)) {
                        continue;
                    }
                }
                match node_kind {
                    WorkspaceNodeKind::Directory => {
                        queue.push_back((
                            relative_to_base,
                            None,
                            ancestors.clone(),
                            ignore_matchers.clone(),
                            inside_git,
                        ));
                    }
                    WorkspaceNodeKind::File => {
                        if !include(&relative_to_base) {
                            continue;
                        }
                        if files.len() >= max_files {
                            limit_reached = true;
                            break;
                        }
                        let file = match collect_file(self, &relative_to_root) {
                            Ok(Some(file)) => file,
                            Ok(None)
                            | Err(WorkspaceOpenError::Escape(_))
                            | Err(WorkspaceOpenError::Unsupported(_)) => continue,
                            Err(WorkspaceOpenError::Inaccessible(_))
                            | Err(WorkspaceOpenError::SymlinkLoop(_)) => {
                                incomplete = true;
                                continue;
                            }
                            Err(error) => return Err(error.to_string()),
                        };
                        files.push(file);
                    }
                }
            }
            if limit_reached {
                break;
            }
        }
        Ok((files, incomplete || limit_reached, ignore_incomplete))
    }

    #[cfg(target_os = "linux")]
    fn git_marker(
        &self,
        relative_directory: &Path,
    ) -> Result<Option<GitMarker>, WorkspaceOpenError> {
        let Some(directory) = self.pin_beneath(relative_directory)? else {
            return Ok(None);
        };
        let marker_path = relative_directory.join(".git");
        let Some(marker) = open_component(
            &directory,
            Path::new(".git"),
            &self.display_path(&marker_path),
            PIN_FLAGS,
        )?
        else {
            return Ok(None);
        };
        marker
            .metadata()
            .map(|metadata| {
                Some(GitMarker {
                    is_directory: metadata.is_dir(),
                })
            })
            .map_err(|error| {
                WorkspaceOpenError::Other(format!(
                    "Failed to inspect Git marker {}: {error}",
                    self.display_path(&marker_path).display()
                ))
            })
    }

    #[cfg(target_os = "macos")]
    fn git_marker(
        &self,
        relative_directory: &Path,
    ) -> Result<Option<GitMarker>, WorkspaceOpenError> {
        let marker_path = relative_directory.join(".git");
        self.pin_relative_node_kind(&marker_path).map(|kind| {
            kind.map(|kind| GitMarker {
                is_directory: kind == WorkspaceNodeKind::Directory,
            })
        })
    }

    fn load_ignore_matchers(
        &self,
        relative_directory: &Path,
        matcher_directory: &Path,
        inside_git: bool,
        git_directory: bool,
        ignore_budget: &mut u64,
    ) -> Result<LoadedIgnoreMatchers, WorkspaceOpenError> {
        let mut loaded = LoadedIgnoreMatchers::default();
        if git_directory {
            record_ignore_file(
                &mut loaded.git_exclude,
                &mut loaded.incomplete,
                self.load_ignore_file(
                    relative_directory,
                    matcher_directory,
                    ".git/info/exclude",
                    ignore_budget,
                ),
            )?;
        }
        if inside_git {
            record_ignore_file(
                &mut loaded.gitignore,
                &mut loaded.incomplete,
                self.load_ignore_file(
                    relative_directory,
                    matcher_directory,
                    ".gitignore",
                    ignore_budget,
                ),
            )?;
        }
        record_ignore_file(
            &mut loaded.ignore,
            &mut loaded.incomplete,
            self.load_ignore_file(
                relative_directory,
                matcher_directory,
                ".ignore",
                ignore_budget,
            ),
        )?;
        record_ignore_file(
            &mut loaded.rgignore,
            &mut loaded.incomplete,
            self.load_ignore_file(
                relative_directory,
                matcher_directory,
                ".rgignore",
                ignore_budget,
            ),
        )?;
        Ok(loaded)
    }

    fn load_ignore_file(
        &self,
        relative_directory: &Path,
        matcher_directory: &Path,
        name: &str,
        ignore_budget: &mut u64,
    ) -> Result<LoadedIgnoreFile, WorkspaceOpenError> {
        let relative_path = relative_directory.join(name);
        let file = match self.open_relative_node_inner(&relative_path) {
            Ok(Some(WorkspaceNode::File(file))) => file,
            Ok(Some(WorkspaceNode::Directory(_))) | Ok(None) => {
                return Ok(LoadedIgnoreFile {
                    matcher: None,
                    incomplete: false,
                });
            }
            Err(error) => return Err(error),
        };
        let read_limit = MAX_IGNORE_FILE_BYTES.min(*ignore_budget);
        if read_limit == 0 {
            return Ok(LoadedIgnoreFile {
                matcher: None,
                incomplete: true,
            });
        }

        let mut builder = GitignoreBuilder::new(matcher_directory);
        let mut incomplete = false;
        let source = Some(file.display_path.clone());
        let mut reader = BufReader::new(file.file.take(read_limit + 1));
        let mut line = Vec::new();
        let mut index = 0;
        let mut bytes_read = 0_u64;
        loop {
            line.clear();
            let read = match reader.read_until(b'\n', &mut line) {
                Ok(read) => read,
                Err(_) => {
                    incomplete = true;
                    break;
                }
            };
            if read == 0 {
                break;
            }
            bytes_read += read as u64;
            if bytes_read > read_limit {
                incomplete = true;
                break;
            }
            trim_ignore_line_ending(&mut line);
            let line = String::from_utf8_lossy(&line);
            incomplete |= matches!(&line, std::borrow::Cow::Owned(_));
            let line = if index == 0 {
                line.trim_start_matches('\u{feff}')
            } else {
                &line
            };
            incomplete |= builder.add_line(source.clone(), line).is_err();
            index += 1;
        }
        *ignore_budget = ignore_budget.saturating_sub(bytes_read.min(read_limit));
        builder
            .build()
            .map(|matcher| LoadedIgnoreFile {
                matcher: Some(matcher),
                incomplete,
            })
            .map_err(|error| {
                WorkspaceOpenError::Other(format!(
                    "Failed to compile ignore rules under {}: {error}",
                    self.display_path(relative_directory).display()
                ))
            })
    }

    fn load_parent_ignore_matchers(
        &self,
        base_path: &Path,
        ignore_budget: &mut u64,
    ) -> Result<(IgnoreMatchers, bool, bool), String> {
        if base_path.as_os_str().is_empty() || base_path == Path::new(".") {
            return Ok((IgnoreMatchers::default(), false, false));
        }
        let mut directories = Vec::new();
        let mut parent = base_path.parent();
        while let Some(path) = parent {
            directories.push(if path.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                path.to_path_buf()
            });
            if path.as_os_str().is_empty() {
                break;
            }
            parent = path.parent();
        }
        directories.reverse();

        let mut matchers = IgnoreMatchers::default();
        let mut inside_git = false;
        let mut incomplete = false;
        for directory in directories {
            let git_directory = match self.git_marker(&directory) {
                Ok(Some(marker)) => {
                    matchers.git_exclude.clear();
                    matchers.gitignore.clear();
                    inside_git = true;
                    marker.is_directory
                }
                Ok(None) => false,
                Err(WorkspaceOpenError::Inaccessible(_))
                | Err(WorkspaceOpenError::Escape(_))
                | Err(WorkspaceOpenError::SymlinkLoop(_)) => {
                    incomplete = true;
                    false
                }
                Err(WorkspaceOpenError::Unsupported(_)) => false,
                Err(error) => return Err(error.to_string()),
            };
            match self.load_ignore_matchers(
                &directory,
                &directory,
                inside_git,
                git_directory,
                ignore_budget,
            ) {
                Ok(loaded) => {
                    incomplete |= loaded.incomplete;
                    matchers.extend(loaded);
                }
                Err(WorkspaceOpenError::Unsupported(_)) => {}
                Err(WorkspaceOpenError::Inaccessible(_))
                | Err(WorkspaceOpenError::Escape(_))
                | Err(WorkspaceOpenError::SymlinkLoop(_)) => incomplete = true,
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok((matchers, inside_git, incomplete))
    }
}

fn normalize_workspace_relative_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(component) => components.push(component.to_os_string()),
            std::path::Component::ParentDir => {
                if components.last().is_some_and(|component| component != "..") {
                    components.pop();
                } else {
                    components.push(OsString::from(".."));
                }
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return path.to_path_buf();
            }
        }
    }
    if components.is_empty() {
        return PathBuf::from(".");
    }
    components.into_iter().collect()
}

fn record_ignore_file(
    target: &mut Option<Gitignore>,
    incomplete: &mut bool,
    result: Result<LoadedIgnoreFile, WorkspaceOpenError>,
) -> Result<(), WorkspaceOpenError> {
    match result {
        Ok(file) => {
            *target = file.matcher;
            *incomplete |= file.incomplete;
        }
        Err(WorkspaceOpenError::Inaccessible(_))
        | Err(WorkspaceOpenError::Escape(_))
        | Err(WorkspaceOpenError::SymlinkLoop(_)) => *incomplete = true,
        Err(WorkspaceOpenError::Unsupported(_)) => {}
        Err(error) => return Err(error),
    }
    Ok(())
}

fn ignore_status(matchers: &IgnoreMatchers, path: &Path, is_directory: bool) -> Option<bool> {
    matcher_status(&matchers.rgignore, path, is_directory)
        .or_else(|| matcher_status(&matchers.ignore, path, is_directory))
        .or_else(|| matcher_status(&matchers.gitignore, path, is_directory))
        .or_else(|| matcher_status(&matchers.git_exclude, path, is_directory))
}

fn matcher_status(matchers: &[Gitignore], path: &Path, is_directory: bool) -> Option<bool> {
    matchers.iter().rev().find_map(|matcher| {
        let matched = matcher.matched(path, is_directory);
        if matched.is_ignore() {
            Some(true)
        } else if matched.is_whitelist() {
            Some(false)
        } else {
            None
        }
    })
}

fn trim_ignore_line_ending(line: &mut Vec<u8>) {
    if line.last() == Some(&b'\n') {
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
    }
}

fn is_permission_error(error: rustix::io::Errno) -> bool {
    matches!(error, rustix::io::Errno::ACCESS | rustix::io::Errno::PERM)
}

#[cfg(target_os = "linux")]
fn reopen_pinned(
    pinned: &File,
    display_path: &Path,
    directory: bool,
) -> Result<Option<File>, WorkspaceOpenError> {
    let descriptor_path = PathBuf::from(format!("/proc/self/fd/{}", pinned.as_raw_fd()));
    let flags = if directory {
        READ_FLAGS | OFlags::DIRECTORY
    } else {
        READ_FLAGS
    };
    match openat2(
        CWD,
        &descriptor_path,
        flags,
        Mode::empty(),
        ResolveFlags::empty(),
    ) {
        Ok(descriptor) => Ok(Some(File::from(descriptor))),
        Err(rustix::io::Errno::ACCESS | rustix::io::Errno::PERM) => {
            Err(WorkspaceOpenError::Inaccessible(display_path.to_path_buf()))
        }
        Err(error) => Err(WorkspaceOpenError::Other(format!(
            "Failed to open {}: {error}",
            display_path.display()
        ))),
    }
}

#[cfg(target_os = "linux")]
fn platform_support_error(error: rustix::io::Errno) -> WorkspaceRootError {
    if error == rustix::io::Errno::NOSYS {
        WorkspaceRootError::Permanent(format!(
            "Workspace-confined read tools require Linux openat2 support \
             (kernel 5.6 or newer): {error}"
        ))
    } else {
        WorkspaceRootError::Permanent(format!(
            "Failed to verify openat2 support for workspace confinement: {error}"
        ))
    }
}

#[cfg(target_os = "linux")]
fn root_path_from_descriptor(
    directory: &File,
    requested_root: &Path,
) -> Result<PathBuf, WorkspaceRootError> {
    let descriptor_path = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
    let root = std::fs::read_link(&descriptor_path).map_err(|error| {
        WorkspaceRootError::Permanent(format!(
            "Failed to identify pinned workspace root {}: {error}",
            requested_root.display()
        ))
    })?;
    let metadata = directory.metadata().map_err(|error| {
        WorkspaceRootError::Permanent(format!(
            "Failed to inspect pinned workspace root {}: {error}",
            requested_root.display()
        ))
    })?;
    if !root.is_absolute() || metadata.nlink() == 0 {
        return Err(WorkspaceRootError::Permanent(format!(
            "Pinned workspace root changed during startup: {}",
            requested_root.display()
        )));
    }
    Ok(root)
}

struct RemainingPath {
    components: VecDeque<OsString>,
    must_be_directory: bool,
}

impl RemainingPath {
    fn new(path: &Path, must_be_directory: bool) -> Result<Self, WorkspaceOpenError> {
        let mut remaining = Self {
            components: VecDeque::new(),
            must_be_directory,
        };
        remaining.prepend(path, false)?;
        Ok(remaining)
    }

    fn prepend(&mut self, path: &Path, must_be_directory: bool) -> Result<(), WorkspaceOpenError> {
        if self.components.is_empty() {
            self.must_be_directory |= must_be_directory;
        }

        let mut components = Vec::new();
        for component in path.components() {
            match component {
                std::path::Component::CurDir => components.push(OsString::from(".")),
                std::path::Component::ParentDir => components.push(OsString::from("..")),
                std::path::Component::Normal(component) => {
                    components.push(component.to_os_string());
                }
                std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                    return Err(WorkspaceOpenError::Other(format!(
                        "Workspace-relative path must not be absolute: {}",
                        path.display()
                    )));
                }
            }
        }
        for component in components.into_iter().rev() {
            self.components.push_front(component);
        }
        Ok(())
    }

    fn pop_front(&mut self) -> Option<OsString> {
        self.components.pop_front()
    }

    fn push_front(&mut self, component: OsString) {
        self.components.push_front(component);
    }

    fn is_empty(&self) -> bool {
        self.components.is_empty()
    }

    fn must_be_directory(&self) -> bool {
        self.must_be_directory
    }
}

fn path_requires_directory(path: &Path) -> bool {
    let bytes = path.as_os_str().as_bytes();
    bytes.ends_with(b"/") || bytes == b"." || bytes.ends_with(b"/.")
}

#[cfg(target_os = "linux")]
fn open_component(
    directory: &File,
    component: &Path,
    display_path: &Path,
    flags: OFlags,
) -> Result<Option<File>, WorkspaceOpenError> {
    match openat2(directory, component, flags, Mode::empty(), RESOLVE_FLAGS) {
        Ok(descriptor) => Ok(Some(File::from(descriptor))),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(rustix::io::Errno::XDEV) => Err(WorkspaceOpenError::Escape(display_path.to_path_buf())),
        Err(rustix::io::Errno::ACCESS | rustix::io::Errno::PERM) => {
            Err(WorkspaceOpenError::Inaccessible(display_path.to_path_buf()))
        }
        Err(error) => Err(WorkspaceOpenError::Other(format!(
            "Failed to open {}: {error}",
            display_path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn accepts_internal_symlinks_and_rejects_escapes() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("inside.txt"), "inside").unwrap();
        std::fs::write(parent.path().join("outside.txt"), "outside").unwrap();
        symlink("inside.txt", root.join("inside-link")).unwrap();
        symlink(root.join("inside.txt"), root.join("absolute-inside-link")).unwrap();
        symlink(parent.path().join("outside.txt"), root.join("outside-link")).unwrap();
        let workspace = WorkspaceFs::new(&root).unwrap();

        let mut inside = workspace.open_file(&root, "inside-link").unwrap();
        let mut content = String::new();
        inside.file.read_to_string(&mut content).unwrap();
        assert_eq!(content, "inside");
        let mut absolute_inside = workspace.open_file(&root, "absolute-inside-link").unwrap();
        content.clear();
        absolute_inside.file.read_to_string(&mut content).unwrap();
        assert_eq!(content, "inside");
        let error = workspace.open_file(&root, "outside-link").unwrap_err();
        assert!(error.contains("escapes workspace root"), "{error}");

        let outside_directory = parent.path().join("outside-directory");
        std::fs::create_dir(&outside_directory).unwrap();
        std::fs::write(outside_directory.join("secret.txt"), "secret").unwrap();
        symlink(&outside_directory, root.join("outside-directory-link")).unwrap();
        let error = workspace
            .open_file(&root, "outside-directory-link/secret.txt")
            .unwrap_err();
        assert!(error.contains("escapes workspace root"), "{error}");
    }

    #[test]
    fn absolute_paths_must_start_inside_the_workspace() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("inside.txt"), "inside").unwrap();
        let outside = parent.path().join("outside.txt");
        std::fs::write(&outside, "outside").unwrap();
        let workspace = WorkspaceFs::new(&root).unwrap();

        assert!(workspace
            .open_file(&root, root.join("inside.txt").to_str().unwrap())
            .is_ok());
        let error = workspace
            .open_file(&root, outside.to_str().unwrap())
            .unwrap_err();
        assert!(error.contains("escapes workspace root"), "{error}");
    }

    #[test]
    fn parent_components_cannot_escape_the_workspace() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(parent.path().join("outside.txt"), "outside").unwrap();
        let workspace = WorkspaceFs::new(&root).unwrap();

        let error = workspace.open_file(&root, "../outside.txt").unwrap_err();

        assert!(error.contains("escapes workspace root"), "{error}");
    }

    #[test]
    fn walker_skips_external_symlinks_and_follows_internal_ones() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("workspace");
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::fs::write(root.join("real/inside.txt"), "inside").unwrap();
        std::fs::write(parent.path().join("outside.txt"), "outside").unwrap();
        symlink(root.join("real"), root.join("internal-dir")).unwrap();
        symlink(parent.path().join("outside.txt"), root.join("outside-link")).unwrap();
        let workspace = WorkspaceFs::new(&root).unwrap();
        let directory = workspace.open_directory(&root, ".").unwrap();

        let walked = workspace.walk_files(directory, 10, |_| true).unwrap();
        let paths = walked
            .files
            .iter()
            .map(|file| file.relative_path.clone())
            .collect::<Vec<_>>();

        assert!(paths.iter().any(|path| path.ends_with("inside.txt")));
        assert!(!paths.iter().any(|path| path.ends_with("outside-link")));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn root_descriptor_survives_path_replacement() {
        let parent = tempfile::tempdir().unwrap();
        let container = parent.path().join("container");
        let original_root = container.join("workspace");
        std::fs::create_dir_all(&original_root).unwrap();
        std::fs::write(original_root.join("value.txt"), "trusted").unwrap();
        let workspace = WorkspaceFs::new(&original_root).unwrap();

        let moved = parent.path().join("moved");
        std::fs::rename(&container, &moved).unwrap();
        let replacement = parent.path().join("replacement");
        std::fs::create_dir_all(replacement.join("workspace")).unwrap();
        std::fs::write(replacement.join("workspace/value.txt"), "outside").unwrap();
        symlink(&replacement, &container).unwrap();

        let mut file = workspace
            .open_file(&original_root, "value.txt")
            .unwrap()
            .file;
        let mut content = String::new();
        file.read_to_string(&mut content).unwrap();
        assert_eq!(content, "trusted");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn root_replacement_fails_closed() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("value.txt"), "trusted").unwrap();
        let workspace = WorkspaceFs::new(&root).unwrap();

        let moved = parent.path().join("moved");
        std::fs::rename(&root, &moved).unwrap();
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("value.txt"), "replacement").unwrap();

        let error = workspace.open_file(&root, "value.txt").unwrap_err();
        assert!(
            error.contains("Pinned workspace root was replaced"),
            "{error}"
        );
    }

    #[test]
    fn root_identity_is_derived_from_the_pinned_descriptor() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        let link = directory.path().join("workspace-link");
        std::fs::create_dir(&target).unwrap();
        symlink(&target, &link).unwrap();

        let workspace = WorkspaceFs::new(&link).unwrap();

        assert_eq!(workspace.root(), target.canonicalize().unwrap());
    }

    #[test]
    fn live_root_name_may_end_with_deleted_marker() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("workspace (deleted)");
        std::fs::create_dir(&root).unwrap();

        let workspace = WorkspaceFs::new(&root).unwrap();

        assert_eq!(workspace.root(), root.canonicalize().unwrap());
    }

    #[test]
    fn configured_root_alias_accepts_absolute_paths() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        let link = directory.path().join("workspace-link");
        let outside = directory.path().join("outside");
        std::fs::create_dir(&target).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(target.join("inside.txt"), "inside").unwrap();
        std::fs::write(outside.join("inside.txt"), "outside").unwrap();
        symlink(&target, &link).unwrap();
        let workspace = WorkspaceFs::new(&link).unwrap();
        std::fs::remove_file(&link).unwrap();
        symlink(&outside, &link).unwrap();

        let file = link.join("inside.txt");
        let mut file = workspace
            .open_file(&target, file.to_str().unwrap())
            .unwrap()
            .file;
        let mut content = String::new();
        file.read_to_string(&mut content).unwrap();

        assert_eq!(content, "inside");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn absolute_symlink_targets_accept_the_configured_root_alias() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        let link = directory.path().join("workspace-link");
        let outside = directory.path().join("outside");
        std::fs::create_dir_all(target.join("releases/v1")).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(target.join("releases/v1/value.txt"), "inside").unwrap();
        std::fs::write(outside.join("value.txt"), "outside").unwrap();
        symlink(&target, &link).unwrap();
        symlink(link.join("releases/v1/value.txt"), target.join("current")).unwrap();
        let workspace = WorkspaceFs::new(&link).unwrap();
        std::fs::remove_file(&link).unwrap();
        symlink(&outside, &link).unwrap();

        let mut file = workspace.open_file(&target, "current").unwrap().file;
        let mut content = String::new();
        file.read_to_string(&mut content).unwrap();

        assert_eq!(content, "inside");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn searchable_root_does_not_require_list_permission_for_exact_reads() {
        use std::os::unix::fs::PermissionsExt;

        if nix::unistd::Uid::effective().as_raw() == 0 {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("readable.txt"), "inside").unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o111)).unwrap();

        let result = WorkspaceFs::new(&root).and_then(|workspace| {
            workspace.open_file(&root, "readable.txt").map(|mut file| {
                let mut content = String::new();
                file.file.read_to_string(&mut content).unwrap();
                content
            })
        });
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();

        assert_eq!(result.unwrap(), "inside");
    }

    #[test]
    fn rejects_nested_mounts() {
        if !Path::new("/proc/self/status").exists() {
            return;
        }
        let workspace = WorkspaceFs::new(Path::new("/")).unwrap();

        let error = workspace
            .open_file(Path::new("/"), "/proc/self/status")
            .unwrap_err();

        assert!(error.contains("escapes workspace root"), "{error}");
    }

    #[test]
    fn walker_does_not_retain_sibling_directory_descriptors() {
        if !Path::new("/proc/self/fd").exists() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        for index in 0..128 {
            let child = directory.path().join(format!("child-{index:03}"));
            std::fs::create_dir(&child).unwrap();
            std::fs::write(child.join("file.txt"), "text").unwrap();
        }
        let workspace = WorkspaceFs::new(directory.path()).unwrap();
        let root = workspace.open_directory(directory.path(), ".").unwrap();
        let mut workspace_descriptors = 0;

        workspace
            .walk_files(root, 1, |_| {
                workspace_descriptors = std::fs::read_dir("/proc/self/fd")
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter_map(|entry| std::fs::read_link(entry.path()).ok())
                    .filter(|path| path.starts_with(directory.path()))
                    .count();
                true
            })
            .unwrap();

        assert!(workspace_descriptors <= 4, "{workspace_descriptors}");
    }

    #[test]
    fn relative_file_discovery_does_not_retain_file_descriptors() {
        if !Path::new("/proc/self/fd").exists() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        for index in 0..100 {
            std::fs::write(directory.path().join(format!("{index:03}.txt")), "text").unwrap();
        }
        let workspace = WorkspaceFs::new(directory.path()).unwrap();
        let root = workspace.open_directory(directory.path(), ".").unwrap();

        let walked = workspace
            .walk_relative_file_paths(root, 100, false, |_| true)
            .unwrap();
        let workspace_descriptors = std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| std::fs::read_link(entry.path()).ok())
            .filter(|path| path.starts_with(directory.path()))
            .count();

        assert_eq!(walked.paths.len(), 100);
        assert!(!walked.truncated);
        assert!(workspace_descriptors <= 2, "{workspace_descriptors}");
    }

    #[test]
    fn special_files_are_classified_before_readable_open() {
        use std::os::unix::net::UnixListener;

        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("service.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        let workspace = WorkspaceFs::new(directory.path()).unwrap();

        let error = workspace
            .open_file(directory.path(), "service.sock")
            .unwrap_err();

        assert!(error.contains("Unsupported filesystem object"), "{error}");
    }

    #[test]
    fn exact_match_limit_is_not_reported_as_truncated() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("one.txt"), "one").unwrap();
        std::fs::write(directory.path().join("two.txt"), "two").unwrap();
        let workspace = WorkspaceFs::new(directory.path()).unwrap();
        let root = workspace.open_directory(directory.path(), ".").unwrap();

        let walked = workspace.walk_file_paths(root, 2, |_| true).unwrap();

        assert_eq!(walked.paths.len(), 2);
        assert!(!walked.truncated);
    }

    #[test]
    fn capped_walk_returns_lexicographically_first_files() {
        let directory = tempfile::tempdir().unwrap();
        for index in (0..10).rev() {
            std::fs::write(directory.path().join(format!("{index:02}.txt")), "text").unwrap();
        }
        let workspace = WorkspaceFs::new(directory.path()).unwrap();
        let root = workspace.open_directory(directory.path(), ".").unwrap();

        let walked = workspace.walk_file_paths(root, 3, |_| true).unwrap();
        let names = walked
            .paths
            .iter()
            .filter_map(|path| path.file_name())
            .collect::<Vec<_>>();

        assert_eq!(names, ["00.txt", "01.txt", "02.txt"]);
        assert!(walked.truncated);
    }

    #[test]
    fn walker_filters_known_regular_files_before_opening() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("included.rs"), "included").unwrap();
        let excluded = directory.path().join("excluded.env");
        std::fs::write(&excluded, "excluded").unwrap();
        std::fs::set_permissions(&excluded, std::fs::Permissions::from_mode(0o000)).unwrap();
        let workspace = WorkspaceFs::new(directory.path()).unwrap();
        let root = workspace.open_directory(directory.path(), ".").unwrap();

        let walked = workspace
            .walk_files(root, 10, |path| {
                path.extension().is_some_and(|extension| extension == "rs")
            })
            .unwrap();

        assert_eq!(walked.files.len(), 1);
        assert!(walked.files[0].relative_path.ends_with("included.rs"));
    }

    #[test]
    fn walker_marks_unreadable_subdirectories_incomplete() {
        use std::os::unix::fs::PermissionsExt;

        if nix::unistd::Uid::effective().as_raw() == 0 {
            return;
        }

        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("visible.rs"), "visible").unwrap();
        let locked = directory.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("hidden.rs"), "hidden").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let workspace = WorkspaceFs::new(directory.path()).unwrap();
        let root = workspace.open_directory(directory.path(), ".").unwrap();

        let walked = workspace.walk_files(root, 10, |_| true);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        let walked = walked.unwrap();

        assert!(walked
            .files
            .iter()
            .any(|file| file.relative_path.ends_with("visible.rs")));
        assert!(!walked
            .files
            .iter()
            .any(|file| file.relative_path.ends_with("hidden.rs")));
        assert!(walked.truncated);
    }

    #[test]
    fn permission_errors_are_recognized_as_inaccessible() {
        assert!(is_permission_error(rustix::io::Errno::ACCESS));
        assert!(is_permission_error(rustix::io::Errno::PERM));
    }

    #[test]
    fn directory_alias_preserves_all_searchable_paths() {
        let directory = tempfile::tempdir().unwrap();
        let real = directory.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("inside.rs"), "inside").unwrap();
        symlink("..", real.join("loop")).unwrap();
        symlink("real", directory.path().join("alias")).unwrap();
        let workspace = WorkspaceFs::new(directory.path()).unwrap();
        let root = workspace.open_directory(directory.path(), ".").unwrap();

        let walked = workspace.walk_file_paths(root, 10, |_| true).unwrap();

        assert_eq!(
            walked.paths,
            vec![
                workspace.root().join("alias/inside.rs"),
                workspace.root().join("real/inside.rs"),
            ]
        );
        assert!(!walked.truncated);
    }

    #[test]
    fn walker_skips_cyclic_symlinks_and_keeps_valid_files() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("valid.txt"), "valid").unwrap();
        symlink("b", directory.path().join("a")).unwrap();
        symlink("a", directory.path().join("b")).unwrap();
        let workspace = WorkspaceFs::new(directory.path()).unwrap();
        let root = workspace.open_directory(directory.path(), ".").unwrap();

        let walked = workspace.walk_file_paths(root, 10, |_| true).unwrap();
        let exact_error = workspace.open_file(directory.path(), "a").unwrap_err();

        assert_eq!(walked.paths.len(), 1);
        assert!(walked.paths[0].ends_with("valid.txt"));
        assert!(walked.truncated);
        assert!(
            exact_error.contains("Too many symbolic links"),
            "{exact_error}"
        );
    }

    #[test]
    fn unknown_entry_classifies_unreadable_file_without_read_access() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let unreadable = directory.path().join("excluded.env");
        std::fs::write(&unreadable, "excluded").unwrap();
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
        let workspace = WorkspaceFs::new(directory.path()).unwrap();

        let kind = workspace
            .classify_walk_entry(Path::new("excluded.env"), FileType::Unknown)
            .unwrap();

        assert!(matches!(kind, Some(WorkspaceNodeKind::File)));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unsupported_openat2_has_an_actionable_startup_error() {
        let error = platform_support_error(rustix::io::Errno::NOSYS).to_string();

        assert!(error.contains("require Linux openat2 support"), "{error}");
        assert!(error.contains("kernel 5.6 or newer"), "{error}");
    }
}
