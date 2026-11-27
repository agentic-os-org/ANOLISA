//! Open package files from a verified root descriptor and check static paths.

use std::collections::VecDeque;
use std::ffi::{CStr, CString, OsString};
use std::fs::{self, File, Metadata};
use std::io::{self, Read};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

#[cfg(target_os = "linux")]
use libc::__errno_location as errno_location;
#[cfg(target_os = "macos")]
use libc::__error as errno_location;
use thiserror::Error;

use crate::DEFAULT_MAX_SKILL_SIZE;

use super::PluginError;

/// Check package-relative resource containment at the time of this call.
///
/// Missing suffix components are handled lexically. An existing link must be
/// resolved even when its target is missing, so it cannot hide an escape.
/// The returned path is for static validation only. Runtime I/O, including
/// package reads through FUSE, must enforce containment by opening relative to
/// a verified directory descriptor at access time to avoid check-to-use races.
pub fn check_package_resource_path(
    plugin_root: &Path,
    rel_path: &Path,
) -> Result<PathBuf, PluginError> {
    if rel_path.has_root() || matches!(rel_path.components().next(), Some(Component::Prefix(_))) {
        return Err(PluginError::AbsoluteResource(rel_path.to_path_buf()));
    }
    let root = canonicalize(plugin_root)?;
    inside_plugin_root(&root.join(rel_path), &root)
}

pub(super) fn canonicalize(path: &Path) -> Result<PathBuf, PluginError> {
    fs::canonicalize(path).map_err(|source| resolution_error(path, source))
}

pub(super) fn inside_plugin_root(path: &Path, root: &Path) -> Result<PathBuf, PluginError> {
    resolve_inside(path, root, false, &mut 0)
}

pub(super) fn inside_plugin_root_dir(path: &Path, root: &Path) -> Result<PathBuf, PluginError> {
    resolve_inside(path, root, true, &mut 0)
}

fn resolve_inside(
    path: &Path,
    root: &Path,
    require_dir: bool,
    links: &mut usize,
) -> Result<PathBuf, PluginError> {
    let mut resolved = PathBuf::new();
    let mut todo = VecDeque::new();
    // Retain the original suffix; strip_prefix can normalize trailing dots.
    let mut start = path;
    if let Ok(relative) = path.strip_prefix(root) {
        resolved.push(root);
        start = relative;
    }
    prepend(start, require_dir || requires_directory(path), &mut todo);
    let outside = || outside_root(path, root);
    while let Some(step) = todo.pop_front() {
        let Step::Segment(part) = step else {
            require_searchable_directory(&resolved)?;
            continue;
        };
        match Path::new(&part).components().next() {
            Some(Component::ParentDir) => {
                if resolved == root && root.parent().is_some() {
                    return Err(outside());
                }
                require_searchable_directory(&resolved)?;
                resolved.pop();
            }
            Some(Component::Normal(_)) => {
                let candidate = resolved.join(part);
                match fs::symlink_metadata(&candidate) {
                    Ok(meta) if meta.is_symlink() => {
                        count_link(path, links)?;
                        let target = fs::read_link(&candidate)
                            .map_err(|source| resolution_error(&candidate, source))?;
                        if target.is_absolute() {
                            resolved.clear();
                        }
                        prepend(&target, requires_directory(&target), &mut todo);
                    }
                    Ok(_) => resolved = canonicalize(&candidate)?,
                    Err(err) if err.kind() == io::ErrorKind::NotFound => resolved = candidate,
                    Err(source) => return Err(resolution_error(&candidate, source)),
                }
            }
            Some(Component::Prefix(_) | Component::RootDir) => resolved.push(part),
            _ => {}
        }
    }
    if resolved.starts_with(root) {
        Ok(resolved)
    } else {
        Err(outside())
    }
}

enum Step {
    Segment(OsString),
    RequireDir,
}

fn requires_directory(path: &Path) -> bool {
    let bytes = path.as_os_str().as_bytes();
    matches!(bytes, b"." | b"..")
        || bytes.ends_with(b"/")
        || bytes.ends_with(b"/.")
        || bytes.ends_with(b"/..")
}

fn prepend(path: &Path, require_dir: bool, todo: &mut VecDeque<Step>) {
    if require_dir {
        todo.push_front(Step::RequireDir);
    }
    for component in path.components().rev() {
        todo.push_front(Step::Segment(component.as_os_str().to_owned()));
    }
}

fn require_searchable_directory(path: &Path) -> Result<(), PluginError> {
    // Looking up "." requires search permission without listing the directory.
    match fs::symlink_metadata(path.join(".")) {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(resolution_error(path, err)),
    }
}

fn resolution_error(path: &Path, source: io::Error) -> PluginError {
    PluginError::ResolvePath {
        path: path.to_path_buf(),
        source,
    }
}

fn outside_root(path: &Path, root: &Path) -> PluginError {
    PluginError::OutsideRoot {
        path: path.to_path_buf(),
        root: root.to_path_buf(),
    }
}

fn count_link(path: &Path, count: &mut usize) -> Result<(), PluginError> {
    *count += 1;
    if *count > 40 {
        return Err(resolution_error(
            path,
            io::Error::other("too many symbolic links"),
        ));
    }
    Ok(())
}

#[derive(Debug, Error)]
pub(super) enum OpenError {
    #[error("package entry is absent")]
    Absent,
    #[error("package entry has an unexpected filesystem kind or a missing target")]
    WrongKind,
    #[error("cannot open package entry: {0}")]
    Unreadable(#[source] io::Error),
    #[error(transparent)]
    Containment(#[from] PluginError),
}

#[derive(Debug, Error)]
pub(super) enum ReadError {
    #[error("file exceeds 1 MiB limit")]
    TooLarge,
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Directory,
    RegularFile,
}

#[cfg(target_os = "linux")]
const SEARCH_DIR: libc::c_int = libc::O_PATH | libc::O_DIRECTORY;
#[cfg(target_os = "macos")]
const SEARCH_DIR: libc::c_int = libc::O_SEARCH;

impl Kind {
    fn flags(self) -> libc::c_int {
        match self {
            Self::Directory => SEARCH_DIR,
            Self::RegularFile => libc::O_RDONLY | libc::O_NONBLOCK,
        }
    }

    fn file_type(self) -> libc::mode_t {
        match self {
            Self::Directory => libc::S_IFDIR,
            Self::RegularFile => libc::S_IFREG,
        }
    }

    fn matches(self, meta: &Metadata) -> bool {
        match self {
            Self::Directory => meta.is_dir(),
            Self::RegularFile => meta.is_file(),
        }
    }
}

// Retain the actual ancestors so a symlink's '..' never reopens directories
// from a pathname or traverses the live parent of a concurrently moved fd.
#[derive(Debug)]
pub(super) struct Opened {
    pub(super) path: PathBuf,
    file: File,
    parent: Option<Rc<Self>>,
}

pub(super) struct PackageRoot(Rc<Opened>);

impl PackageRoot {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        let name = os_path(path)?;
        let file = File::from(open_at(libc::AT_FDCWD, &name, SEARCH_DIR)?);
        let path = fs::canonicalize(path)?;
        let opened = file.metadata()?;
        let named = fs::metadata(&path)?;
        if (opened.dev(), opened.ino()) != (named.dev(), named.ino()) {
            return Err(io::Error::other("package root changed while opening"));
        }
        let root = Opened {
            path,
            file,
            parent: None,
        };
        root.check_path(&root.path)?;
        Ok(Self(Rc::new(root)))
    }

    pub(super) fn path(&self) -> &Path {
        &self.0.path
    }

    pub(super) fn open_entry(&self, name: &Path, kind: Kind) -> Result<Rc<Opened>, OpenError> {
        self.open_below(&self.0, name, kind)
    }

    pub(super) fn open_below(
        &self,
        start: &Rc<Opened>,
        name: &Path,
        kind: Kind,
    ) -> Result<Rc<Opened>, OpenError> {
        let root = self.path();
        let mut current = Rc::clone(start);
        let mut todo = VecDeque::new();
        let mut links = 0;
        prepend(name, requires_directory(name), &mut todo);
        while let Some(step) = todo.pop_front() {
            let Step::Segment(part) = step else {
                // Any segment preceding this step was opened as a directory.
                // The start and retained ancestor descriptors are directories too.
                continue;
            };
            match Path::new(&part).components().next() {
                Some(Component::Normal(_)) => {}
                Some(Component::CurDir) => continue,
                Some(Component::ParentDir) => {
                    let parent = current
                        .parent
                        .as_ref()
                        .ok_or_else(|| outside_root(name, root))?;
                    fstat_at(current.file.as_raw_fd(), c".").map_err(OpenError::Unreadable)?;
                    current = Rc::clone(parent);
                    continue;
                }
                _ => return Err(outside_root(name, root).into()),
            }

            current
                .check_path(root)
                .map_err(|err| resolution_error(&current.path, err))?;
            let path = current.path.join(&part);
            let part = os_path(Path::new(&part)).map_err(|err| resolution_error(&path, err))?;
            let candidate_kind = if todo.is_empty() {
                kind
            } else {
                Kind::Directory
            };
            let parent_fd = current.file.as_raw_fd();
            match open_step(parent_fd, &part, &path, links, candidate_kind)? {
                StepOutcome::Symlink(target) => {
                    count_link(&path, &mut links)?;
                    let require_dir = requires_directory(&target);
                    if target.is_absolute() {
                        let resolved;
                        let relative = match target.strip_prefix(root) {
                            Ok(relative) => relative,
                            Err(_) => {
                                // Canonical root aliases (such as /tmp on macOS)
                                // still restart from the verified descriptor.
                                resolved = resolve_inside(&target, root, false, &mut links)?;
                                resolved
                                    .strip_prefix(root)
                                    .map_err(|_| outside_root(&target, root))?
                            }
                        };
                        current = Rc::clone(&self.0);
                        prepend(relative, require_dir, &mut todo);
                    } else {
                        prepend(&target, require_dir, &mut todo);
                    }
                }
                StepOutcome::Opened(file) => {
                    let opened = Opened {
                        path,
                        file,
                        parent: Some(current),
                    };
                    opened
                        .check_path(root)
                        .map_err(|err| resolution_error(&opened.path, err))?;
                    current = Rc::new(opened);
                }
            }
        }
        // A link may end in '.' or '..', in which case the final fd was
        // already opened as a directory earlier in traversal.
        current
            .check_path(root)
            .map_err(|err| resolution_error(&current.path, err))?;
        let meta = current.file.metadata().map_err(OpenError::Unreadable)?;
        if !kind.matches(&meta) {
            return Err(OpenError::WrongKind);
        }
        Ok(current)
    }
}

impl Opened {
    fn check_path(&self, root: &Path) -> io::Result<()> {
        let actual = fd_path(&self.file)?;
        if actual == self.path && actual.starts_with(root) {
            Ok(())
        } else {
            Err(io::Error::other("opened entry moved from its package path"))
        }
    }

    pub(super) fn names(&self, root: &PackageRoot) -> io::Result<Vec<OsString>> {
        self.check_path(root.path())?;
        let mut names = directory_names(&self.file)?;
        self.check_path(root.path())?;
        names.sort_unstable();
        Ok(names)
    }

    pub(super) fn read_regular(
        &self,
        root: &PackageRoot,
    ) -> Result<(Vec<u8>, Metadata), ReadError> {
        self.check_path(root.path())?;
        let metadata = self.file.metadata()?;
        let limit = DEFAULT_MAX_SKILL_SIZE as u64;
        if !metadata.is_file() {
            return Err(
                io::Error::new(io::ErrorKind::InvalidData, "expected a regular file").into(),
            );
        }
        if metadata.len() > limit {
            return Err(ReadError::TooLarge);
        }
        let mut bytes = Vec::new();
        (&self.file).take(limit + 1).read_to_end(&mut bytes)?;
        self.check_path(root.path())?;
        if bytes.len() > DEFAULT_MAX_SKILL_SIZE {
            return Err(ReadError::TooLarge);
        }
        Ok((bytes, metadata))
    }
}

fn os_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))
}

enum StepOutcome {
    Opened(File),
    Symlink(PathBuf),
}

#[cfg(target_os = "linux")]
fn open_step(
    parent_fd: RawFd,
    part: &CStr,
    path: &Path,
    links: usize,
    candidate_kind: Kind,
) -> Result<StepOutcome, OpenError> {
    // Pin the dentry with O_PATH | O_NOFOLLOW before reading link targets or
    // re-opening regular files, so swapped device/FIFO nodes are never opened.
    let path_fd =
        open_at(parent_fd, part, libc::O_PATH | libc::O_NOFOLLOW).map_err(|err| {
            match err.kind() {
                io::ErrorKind::NotFound if links == 0 => OpenError::Absent,
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => OpenError::WrongKind,
                _ => OpenError::Unreadable(err),
            }
        })?;
    let path_file = File::from(path_fd);
    let stat = fstat_fd(path_file.as_raw_fd()).map_err(OpenError::Unreadable)?;
    let file_type = stat.st_mode & libc::S_IFMT;
    if file_type == libc::S_IFLNK {
        let target = read_link_at(path_file.as_raw_fd(), c"")
            .map_err(|err| OpenError::Containment(resolution_error(path, err)))?;
        return Ok(StepOutcome::Symlink(target));
    }
    if file_type != candidate_kind.file_type() {
        return Err(OpenError::WrongKind);
    }
    let file = match candidate_kind {
        Kind::Directory => path_file,
        Kind::RegularFile => {
            let proc_name = os_path(&proc_fd(&path_file)).map_err(OpenError::Unreadable)?;
            let fd = open_at(libc::AT_FDCWD, &proc_name, candidate_kind.flags()).map_err(
                |err| match err.raw_os_error() {
                    Some(libc::EISDIR | libc::ENOTDIR | libc::ENOENT | libc::ENXIO) => {
                        OpenError::WrongKind
                    }
                    _ => OpenError::Unreadable(err),
                },
            )?;
            File::from(fd)
        }
    };
    if !candidate_kind.matches(&file.metadata().map_err(OpenError::Unreadable)?) {
        return Err(OpenError::WrongKind);
    }
    Ok(StepOutcome::Opened(file))
}

#[cfg(target_os = "macos")]
fn open_step(
    parent_fd: RawFd,
    part: &CStr,
    path: &Path,
    links: usize,
    candidate_kind: Kind,
) -> Result<StepOutcome, OpenError> {
    // Linux pins directory and probe descriptors with O_PATH | O_NOFOLLOW.
    // On macOS, fstatat(AT_SYMLINK_NOFOLLOW) and openat(O_NOFOLLOW) look up
    // the name separately, so a concurrent replacement can open a special
    // file. The post-open fstat rejects unexpected types before return or read.
    let stat = fstat_at(parent_fd, part).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound if links == 0 => OpenError::Absent,
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => OpenError::WrongKind,
        _ => OpenError::Unreadable(err),
    })?;
    let file_type = stat.st_mode & libc::S_IFMT;
    if file_type == libc::S_IFLNK {
        let target = read_link_at(parent_fd, part).map_err(|err| resolution_error(path, err))?;
        return Ok(StepOutcome::Symlink(target));
    }
    if file_type != candidate_kind.file_type() {
        return Err(OpenError::WrongKind);
    }
    let fd = open_at(parent_fd, part, candidate_kind.flags() | libc::O_NOFOLLOW).map_err(
        |err| match err.raw_os_error() {
            Some(libc::ELOOP | libc::EISDIR | libc::ENOTDIR | libc::ENOENT | libc::ENXIO) => {
                OpenError::WrongKind
            }
            _ => OpenError::Unreadable(err),
        },
    )?;
    let file = File::from(fd);
    if !candidate_kind.matches(&file.metadata().map_err(OpenError::Unreadable)?) {
        return Err(OpenError::WrongKind);
    }
    Ok(StepOutcome::Opened(file))
}

fn open_at(parent: RawFd, name: &CStr, flags: libc::c_int) -> io::Result<OwnedFd> {
    // SAFETY: name is a live C string, parent is a live fd or AT_FDCWD, and
    // flags do not include O_CREAT, so openat takes no mode argument.
    let fd = unsafe { libc::openat(parent, name.as_ptr(), libc::O_CLOEXEC | flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a fresh descriptor whose ownership is unique.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(target_os = "linux")]
fn fstat_fd(fd: RawFd) -> io::Result<libc::stat> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fd is a live descriptor and stat is writable storage for fstat.
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstat initialized the stat struct.
    Ok(unsafe { stat.assume_init() })
}

fn fstat_at(parent: RawFd, name: &CStr) -> io::Result<libc::stat> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: parent and name are live, and stat is writable storage for
    // fstatat's result. AT_SYMLINK_NOFOLLOW inspects the link itself.
    let rc = unsafe {
        libc::fstatat(
            parent,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatat initialized the stat struct.
    Ok(unsafe { stat.assume_init() })
}

fn read_link_at(parent: RawFd, name: &CStr) -> io::Result<PathBuf> {
    let mut bytes = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: parent and name are live, and bytes is a writable buffer of
    // the stated length. readlinkat does not write a trailing NUL.
    let len = unsafe {
        libc::readlinkat(
            parent,
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    if len < 0 {
        return Err(io::Error::last_os_error());
    }
    if len as usize == bytes.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "symbolic link target is too long",
        ));
    }
    bytes.truncate(len as usize);
    Ok(OsString::from_vec(bytes).into())
}

#[cfg(target_os = "linux")]
fn proc_fd(file: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

#[cfg(target_os = "linux")]
fn fd_path(file: &File) -> io::Result<PathBuf> {
    fs::read_link(proc_fd(file))
}

#[cfg(target_os = "macos")]
fn fd_path(file: &File) -> io::Result<PathBuf> {
    let mut bytes = [0u8; libc::MAXPATHLEN as usize];
    // SAFETY: file is live and the buffer is the required MAXPATHLEN bytes.
    let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, bytes.as_mut_ptr()) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    let path = CStr::from_bytes_until_nul(&bytes)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    Ok(OsString::from_vec(path.to_bytes().to_vec()).into())
}

fn directory_names(file: &File) -> io::Result<Vec<OsString>> {
    use std::os::fd::IntoRawFd;
    use std::ptr::NonNull;

    struct DirStream(NonNull<libc::DIR>);

    impl Drop for DirStream {
        fn drop(&mut self) {
            // SAFETY: DirStream uniquely owns this successful fdopendir result.
            unsafe {
                libc::closedir(self.0.as_ptr());
            }
        }
    }

    let fd = open_at(
        file.as_raw_fd(),
        c".",
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
    )?;
    // SAFETY: fd is an open directory fd and is relinquished only on success.
    let raw = unsafe { libc::fdopendir(fd.as_raw_fd()) };
    let stream = DirStream(NonNull::new(raw).ok_or_else(io::Error::last_os_error)?);
    let _ = fd.into_raw_fd();
    let mut names = Vec::new();
    loop {
        // SAFETY: errno is thread-local and stream remains valid. readdir's
        // name is NUL-terminated and is copied before another call.
        let entry = unsafe {
            *errno_location() = 0;
            libc::readdir(stream.0.as_ptr())
        };
        if entry.is_null() {
            let err = io::Error::last_os_error();
            return if err.raw_os_error() == Some(0) {
                Ok(names)
            } else {
                Err(err)
            };
        }
        // SAFETY: readdir returned a live entry with a NUL-terminated name.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if !matches!(name, b"." | b"..") {
            names.push(OsString::from_vec(name.to_vec()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moved_directory_is_refused_by_existing_descriptors() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = tempfile::tempdir()?;
        let location = fixture.path().join("package");
        let skills = location.join("skills");
        fs::create_dir_all(&skills)?;
        fs::write(skills.join("notes"), "original notes")?;
        let root = PackageRoot::open(&location)?;
        let dir = root.open_entry(Path::new("skills"), Kind::Directory)?;
        let notes = root.open_below(&dir, Path::new("notes"), Kind::RegularFile)?;
        assert_eq!(dir.names(&root)?, ["notes"]);

        fs::rename(&skills, fixture.path().join("relocated"))?;
        fs::create_dir(&skills)?;
        fs::write(skills.join("notes"), "replacement notes")?;

        assert!(dir.names(&root).is_err());
        assert!(notes.read_regular(&root).is_err());
        assert!(matches!(
            root.open_below(&dir, Path::new("notes"), Kind::RegularFile),
            Err(OpenError::Containment(_))
        ));
        let current = root.open_entry(Path::new("skills/notes"), Kind::RegularFile)?;
        assert_eq!(current.read_regular(&root)?.0, b"replacement notes");
        Ok(())
    }
}
