//! Strict, default-deny execution through a pinned bubblewrap mount plan.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};

use crate::error::{Result, SandboxError};
use asc_sandbox_types::{
    BackendCapabilities, BackendStatus, ExecutionSpec, FileRight, NetworkMode, PathFact,
    RuleEffect, Validate,
};
use serde::{Deserialize, Serialize};

const MAX_SPEC_BYTES: u64 = 4 * 1024 * 1024;
const MOUNT_ATTR_NOEXEC: u64 = 0x8;
const AT_RECURSIVE: libc::c_uint = 0x8000;

#[derive(Debug)]
struct Object {
    path: String,
    canonical: PathBuf,
    file: File,
    directory: bool,
}

#[derive(Debug)]
struct Mount {
    path: String,
    source: File,
    write: bool,
}

enum MountOperation {
    Bind(Mount),
    Hide(String, bool),
    SearchDirectory(String),
}

impl MountOperation {
    fn path(&self) -> &str {
        match self {
            Self::Bind(mount) => &mount.path,
            Self::Hide(path, _) | Self::SearchDirectory(path) => path,
        }
    }
}

#[derive(Debug)]
struct Prepared {
    mounts: Vec<Mount>,
    hidden: Vec<(String, bool)>,
    aliases: Vec<(PathBuf, PathBuf)>,
    root_writable: bool,
    noexec: Vec<String>,
    search_dirs: BTreeSet<String>,
}

fn unsupported(message: impl Into<String>) -> SandboxError {
    SandboxError::UnsupportedOperation(message.into())
}

/// Spawn a child bound to its launching thread with explicitly inherited descriptors.
///
/// Descriptors must be above stderr, remain open through this call, and already
/// have `FD_CLOEXEC` set. The parent flags never change; only the forked child
/// clears them immediately before exec. The child is killed if its launching
/// thread exits. Existing command hooks still apply.
///
/// # Errors
/// Reject closed, standard-I/O, or non-CLOEXEC descriptors and propagate spawn errors.
pub fn spawn_with_fds(command: &mut Command, inherited_fds: &[RawFd]) -> std::io::Result<Child> {
    for fd in inherited_fds {
        if *fd <= 2 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "inherited descriptors must be above stderr",
            ));
        }
        let flags = unsafe { libc::fcntl(*fd, libc::F_GETFD) };
        if flags == -1 {
            return Err(std::io::Error::last_os_error());
        }
        if flags & libc::FD_CLOEXEC == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "inherited descriptors must have FD_CLOEXEC set in the parent",
            ));
        }
    }
    bind_parent_lifetime(command);
    let inherited_fds = inherited_fds.to_vec();
    // The child hook uses only async-signal-safe fcntl calls. All allocation
    // and descriptor validation occur in the parent before fork.
    unsafe {
        command.pre_exec(move || {
            for fd in &inherited_fds {
                let flags = libc::fcntl(*fd, libc::F_GETFD);
                if flags == -1 || libc::fcntl(*fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    command.spawn()
}

fn bind_parent_lifetime(command: &mut Command) {
    let parent = std::process::id();
    // Bind before exec/setup; bubblewrap takes over this link after it starts.
    // The PID check closes the window where the parent died before prctl.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() as u32 != parent {
                return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
}

/// Recognize the private fd protocol before parsing the legacy CLI.
///
/// # Errors
/// Malformed descriptors and unsupported setup fail before target execution.
pub fn try_run() -> Option<i32> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|s| s == "--capabilities") {
        let result = capabilities().and_then(|capabilities| {
            serde_json::to_writer(std::io::stdout(), &capabilities)
                .map_err(|e| unsupported(e.to_string()))?;
            println!();
            Ok(0)
        });
        return Some(match result {
            Ok(code) => code,
            Err(error) => {
                eprintln!("{error}");
                125
            }
        });
    }
    let inner = args.get(1).is_some_and(|s| s == "--strict-inner");
    if !inner && args.get(1).is_none_or(|s| s != "--execution-spec-fd") {
        return None;
    }
    let options = if inner { &args[2..] } else { &args[1..] };
    let parsed = parse_fds(options);
    let (spec_fd, status_fd) = match parsed {
        Ok(fds) => fds,
        Err(error) => {
            eprintln!("{error}");
            return Some(125);
        }
    };
    // The protocol transfers ownership of these descriptors to this process.
    let mut status = unsafe { File::from_raw_fd(status_fd) };
    let result = (|| {
        let mut input = unsafe { File::from_raw_fd(spec_fd) };
        validate_regular_fd(&input)?;
        if inner {
            let seals = unsafe { libc::fcntl(input.as_raw_fd(), libc::F_GET_SEALS) };
            let required =
                libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
            if seals < 0 || seals & required != required {
                return Err(unsupported("inner execution spec is not sealed"));
            }
        }
        input.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        (&mut input)
            .take(MAX_SPEC_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_SPEC_BYTES {
            return Err(unsupported("execution spec exceeds 4 MiB"));
        }
        drop(input);
        if inner {
            let launch: InnerLaunch = serde_json::from_slice(&bytes)
                .map_err(|e| unsupported(format!("invalid inner launch: {e}")))?;
            launch
                .launch
                .validate()
                .map_err(|e| unsupported(e.to_string()))?;
            if launch.noexec.len() > 1024 {
                return Err(unsupported("too many noexec paths"));
            }
            for path in launch.noexec.iter().chain(launch.mounts.keys()) {
                asc_sandbox_types::validate_os_path(path)
                    .map_err(|e| unsupported(e.to_string()))?;
            }
            verify_installed_mounts(&launch.mounts)?;
            run_inner(&launch, &mut status)
        } else {
            let spec: ExecutionSpec = serde_json::from_slice(&bytes)
                .map_err(|e| unsupported(format!("invalid execution spec: {e}")))?;
            spec.validate()
                .map_err(|e| unsupported(format!("invalid execution spec: {e}")))?;
            run_outer(&spec, &mut status)
        }
    })();
    match result {
        Ok(code) => Some(code),
        Err(error) => {
            let _ = emit(
                &mut status,
                &BackendStatus::Failed {
                    message: error.to_string(),
                },
            );
            // Rejected standard I/O may itself be unwritable (for example, a directory).
            let _ = writeln!(std::io::stderr(), "linux-sandbox: {error}");
            Some(125)
        }
    }
}

fn parse_fds(args: &[String]) -> Result<(RawFd, RawFd)> {
    if args.len() != 4 || args[0] != "--execution-spec-fd" || args[2] != "--status-fd" {
        return Err(unsupported(
            "expected --execution-spec-fd FD --status-fd FD",
        ));
    }
    let parse = |s: &str| {
        s.parse::<RawFd>()
            .ok()
            .filter(|fd| *fd > 2)
            .ok_or_else(|| unsupported("protocol descriptors must be distinct and above stderr"))
    };
    let input = parse(&args[1])?;
    let status = parse(&args[3])?;
    if input == status {
        return Err(unsupported("spec and status descriptors must differ"));
    }
    for fd in [input, status] {
        if unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
            return Err(unsupported("protocol descriptor is not open"));
        }
    }
    Ok((input, status))
}

fn validate_regular_fd(file: &File) -> Result<()> {
    if !file.metadata()?.is_file() {
        return Err(unsupported(
            "execution spec must be a regular file or memfd",
        ));
    }
    Ok(())
}

fn open_object(path: &str, facts: &[PathFact]) -> Result<Object> {
    let canonical = std::fs::canonicalize(path)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
        .open(&canonical)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(unsupported(format!("special filesystem object: {path}")));
    }
    let fact = facts.iter().find(|f| f.path == path).ok_or_else(|| {
        unsupported(format!(
            "missing canonical path fact: {}",
            canonical.display()
        ))
    })?;
    if Path::new(&fact.canonical_path) != canonical
        || fact.device != metadata.dev()
        || fact.inode != metadata.ino()
        || fact.is_directory != metadata.is_dir()
    {
        return Err(unsupported(format!("path identity changed: {path}")));
    }
    Ok(Object {
        path: path.into(),
        canonical,
        file,
        directory: metadata.is_dir(),
    })
}

fn covers(object: &Object, path: &Path) -> bool {
    path == object.canonical || (object.directory && path.starts_with(&object.canonical))
}

fn access(spec: &ExecutionSpec, objects: &[Object], path: &Path) -> (bool, bool) {
    let mut read = false;
    let mut write = false;
    let mut deny_read = false;
    let mut deny_write = false;
    for (rule, object) in spec.permissions.filesystem.rules.iter().zip(objects) {
        if !covers(object, path)
            || rule
                .excluded_paths
                .iter()
                .any(|excluded| path.starts_with(excluded))
        {
            continue;
        }
        match rule.effect {
            RuleEffect::Allow => {
                read |= rule.rights.contains(&FileRight::Read)
                    || rule.rights.contains(&FileRight::Write);
                write |= rule.rights.contains(&FileRight::Write);
            }
            RuleEffect::Deny => {
                deny_read |= rule.rights.contains(&FileRight::Read);
                deny_write |= rule.rights.contains(&FileRight::Write);
            }
        }
    }
    (read && !deny_read, write && !deny_read && !deny_write)
}

fn reserved(path: &Path) -> bool {
    path.starts_with("/proc") || path.starts_with("/dev")
}

fn open_parent<'a>(path: &Path, objects: impl Iterator<Item = &'a Object>) -> Result<File> {
    let anchor = objects
        .filter(|object| object.directory && path.starts_with(&object.canonical))
        .max_by_key(|object| object.canonical.components().count())
        .ok_or_else(|| unsupported(format!("no pinned ancestor for {}", path.display())))?;
    let relative = path
        .strip_prefix(&anchor.canonical)
        .map_err(|error| unsupported(error.to_string()))?;
    let mut directory = anchor.file.try_clone()?;
    // Resolve from the verified object, never reopen its possibly replaced pathname.
    // Each component is opened separately so intermediate symlinks cannot redirect us.
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(unsupported(
                "ancestor path must stay beneath its pinned root",
            ));
        };
        let name = CString::new(name.as_bytes()).map_err(|error| unsupported(error.to_string()))?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd == -1 {
            return Err(unsupported(format!(
                "cannot pin ancestor {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            )));
        }
        // openat returned a new owned descriptor; the preceding directory stays open until here.
        directory = unsafe { File::from_raw_fd(fd) };
    }
    Ok(directory)
}

/// Checks the same pinned mount plan used at installation, before prompting a user.
///
/// # Errors
/// Rejects path identities and restrictions that cannot be installed exactly.
pub fn check_mount_plan(spec: &ExecutionSpec) -> Result<()> {
    prepare(spec).map(drop)
}

// Only synthesize links in the private tmpfs. Never create an entry in a host bind.
fn mount_aliases(
    spec: &ExecutionSpec,
    objects: &[Object],
    mounts: &[Mount],
    hidden: &[(String, bool)],
) -> Result<Vec<(PathBuf, PathBuf)>> {
    let mut links = BTreeMap::new();
    for alias in &spec.mount_aliases {
        let pinned = open_object(alias, &spec.path_facts)?;
        let name = Path::new(alias);
        if name == pinned.canonical || reserved(name) || reserved(&pinned.canonical) {
            return Err(unsupported(format!("invalid mount alias: {alias}")));
        }
        if !access(spec, objects, &pinned.canonical).0 {
            continue;
        }
        let destinations = mounts
            .iter()
            .map(|m| Path::new(&m.path))
            .chain(hidden.iter().map(|(p, _)| Path::new(p)));
        if destinations.clone().any(|p| name.starts_with(p)) {
            continue; // The host-backed parent already supplies this directory entry.
        }
        if destinations.clone().any(|p| p.starts_with(name)) {
            return Err(unsupported(format!(
                "mount alias would cover a restriction: {alias}"
            )));
        }
        links.insert(name.to_owned(), pinned.canonical);
    }
    // A nested destination could follow an earlier link into a host-backed directory.
    if links.keys().any(|p| {
        p.ancestors()
            .skip(1)
            .any(|ancestor| links.contains_key(ancestor))
    }) {
        return Err(unsupported("overlapping synthetic mount aliases"));
    }
    Ok(links.into_iter().collect())
}

fn prepare(spec: &ExecutionSpec) -> Result<Prepared> {
    let objects = spec
        .permissions
        .filesystem
        .rules
        .iter()
        .map(|rule| open_object(&rule.path, &spec.path_facts))
        .collect::<Result<Vec<_>>>()?;
    let mut mounts = BTreeMap::new();
    let mut hidden = Vec::new();
    let mut excluded_objects = Vec::new();
    for (rule, object) in spec.permissions.filesystem.rules.iter().zip(&objects) {
        if reserved(Path::new(&object.path)) || reserved(&object.canonical) {
            return Err(unsupported(format!(
                "reserved sandbox infrastructure path: {} (resolved to {})",
                object.path,
                object.canonical.display()
            )));
        }
        if rule.effect == RuleEffect::Deny {
            if Path::new(&object.path) != object.canonical {
                return Err(unsupported(format!(
                    "deny path must not traverse symlinks: {}",
                    object.path
                )));
            }
            for excluded in &rule.excluded_paths {
                let child = open_object(excluded, &spec.path_facts)?;
                if !object.directory
                    || !child.directory
                    || child.canonical != Path::new(excluded)
                    || reserved(&child.canonical)
                {
                    return Err(unsupported(format!(
                        "subtree exclusion boundaries must be canonical directories: {excluded}"
                    )));
                }
                let (read, write) = access(spec, &objects, &child.canonical);
                if read {
                    mounts.insert(
                        excluded.clone(),
                        Mount {
                            path: excluded.clone(),
                            source: child.file.try_clone()?,
                            write,
                        },
                    );
                }
                excluded_objects.push(child);
            }
            reject_hardlinks(&object.canonical, &rule.excluded_paths)?;
            for alias in &objects {
                if Path::new(&alias.path) != alias.canonical
                    && (covers(alias, &object.canonical) || covers(object, &alias.canonical))
                {
                    return Err(unsupported(
                        "deny intersects an explicitly mounted path alias",
                    ));
                }
            }
        }
        let (read, write) = access(spec, &objects, &object.canonical);
        if read {
            mounts.insert(
                object.path.clone(),
                Mount {
                    path: object.path.clone(),
                    source: object.file.try_clone()?,
                    write,
                },
            );
        } else if rule.effect == RuleEffect::Deny {
            hidden.push((object.path.clone(), object.directory));
        }
    }
    // Pin every parent of a denied carveout. Otherwise an allowed rename of
    // that parent could move the mount away from its protected pathname.
    let mut constrained = objects
        .iter()
        .zip(&spec.permissions.filesystem.rules)
        .filter_map(|(object, rule)| {
            (rule.effect == RuleEffect::Deny).then_some(object.canonical.clone())
        })
        .collect::<Vec<_>>();
    for path in &spec.permissions.execution.deny_paths {
        constrained.push(open_object(path, &spec.path_facts)?.canonical);
    }
    for path in constrained {
        for parent in path.ancestors().skip(1) {
            let (read, write) = access(spec, &objects, parent);
            if !read || parent == Path::new("/") {
                break;
            }
            let key = parent
                .to_str()
                .ok_or_else(|| unsupported("non-UTF8 parent path"))?
                .to_owned();
            if let std::collections::btree_map::Entry::Vacant(entry) = mounts.entry(key.clone()) {
                entry.insert(Mount {
                    path: key,
                    source: open_parent(parent, objects.iter().chain(&excluded_objects))?,
                    write,
                });
            }
        }
    }
    let mut noexec = Vec::new();
    for path in &spec.permissions.execution.deny_paths {
        let object = open_object(path, &spec.path_facts)?;
        if reserved(Path::new(path)) || Path::new(path) != object.canonical {
            return Err(unsupported(format!(
                "noexec path must be canonical and outside infrastructure: {path}"
            )));
        }
        let (read, write) = access(spec, &objects, &object.canonical);
        if read {
            mounts.insert(
                path.clone(),
                Mount {
                    path: path.clone(),
                    source: object.file,
                    write,
                },
            );
            noexec.push(path.clone());
        } else {
            // Restored subtrees still inherit an unreadable ancestor's noexec ceiling.
            noexec.extend(
                mounts
                    .values()
                    .filter(|mount| Path::new(&mount.path).starts_with(&object.canonical))
                    .map(|mount| mount.path.clone()),
            );
        }
    }
    let program = open_object(&spec.launch.resolved_program, &spec.path_facts)?;
    let cwd = open_object(&spec.launch.cwd, &spec.path_facts)?;
    if program.directory
        || !cwd.directory
        || !access(spec, &objects, &program.canonical).0
        || !access(spec, &objects, &cwd.canonical).0
    {
        return Err(unsupported(
            "program and cwd must have explicit readable permissions",
        ));
    }
    if noexec
        .iter()
        .any(|root| program.canonical.starts_with(root))
    {
        return Err(unsupported("initial executable is within a noexec range"));
    }
    let mount_alias = objects
        .iter()
        .chain([&program, &cwd])
        .find(|object| {
            Path::new(&object.path) != object.canonical
                && access(spec, &objects, &object.canonical).0
        })
        .map(|object| object.path.clone());
    for object in [program, cwd] {
        let write = access(spec, &objects, &object.canonical).1;
        mounts.insert(
            object.path.clone(),
            Mount {
                path: object.path,
                source: object.file,
                write,
            },
        );
    }
    let mut mounts: Vec<_> = mounts.into_values().collect();
    mounts.sort_by_key(|mount| Path::new(&mount.path).components().count());
    hidden.sort_by_key(|(path, _)| Path::new(path).components().count());
    let mut hidden_roots: Vec<(String, bool)> = Vec::new();
    for (path, directory) in hidden {
        if !hidden_roots.iter().any(|(parent, is_directory)| {
            *is_directory
                && Path::new(&path).starts_with(parent)
                && !mounts.iter().any(|mount| {
                    mount.path != *parent
                        && Path::new(&mount.path).starts_with(parent)
                        && Path::new(&path).starts_with(&mount.path)
                })
        }) {
            hidden_roots.push((path, directory));
        }
    }
    let aliases = mount_aliases(spec, &objects, &mounts, &hidden_roots)?;
    let mut search_dirs = BTreeSet::new();
    for mount in &mounts {
        for parent in Path::new(&mount.path).ancestors().skip(1) {
            if hidden_roots
                .iter()
                .any(|(hidden, directory)| *directory && parent.starts_with(hidden))
                && !access(spec, &objects, parent).0
            {
                search_dirs.insert(
                    parent
                        .to_str()
                        .ok_or_else(|| unsupported("non-UTF8 parent path"))?
                        .to_owned(),
                );
            }
        }
    }
    // The installed traversal view requires symlink-free mount destinations.
    if !search_dirs.is_empty()
        && let Some(path) = mount_alias
    {
        return Err(unsupported(format!(
            "search directories require canonical mount destinations: {path}"
        )));
    }
    Ok(Prepared {
        aliases,
        mounts,
        hidden: hidden_roots,
        root_writable: access(spec, &objects, Path::new("/")).1,
        noexec,
        search_dirs,
    })
}

fn reject_hardlinks(path: &Path, excluded: &[String]) -> Result<()> {
    if excluded.iter().any(|root| path.starts_with(root)) {
        return Ok(());
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.is_file() && metadata.nlink() > 1 {
        return Err(unsupported(format!(
            "denied object has hardlink aliases: {}",
            path.display()
        )));
    }
    if metadata.is_dir() {
        for child in std::fs::read_dir(path)? {
            reject_hardlinks(&child?.path(), excluded)?;
        }
    }
    Ok(())
}

fn checked_helper() -> Result<File> {
    let helper = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC)
        .open("/proc/self/exe")?;
    validate_static_elf(&helper)?;
    Ok(helper)
}

fn validate_static_elf(file: &File) -> Result<()> {
    let invalid = || unsupported("strict helper requires a static ELF; use make build-sandbox");
    let length = file.metadata()?.len();
    let mut header = [0_u8; 64];
    file.read_exact_at(&mut header, 0).map_err(|_| invalid())?;
    if &header[..4] != b"\x7fELF" || header[4..7] != [2, 1, 1] {
        return Err(invalid());
    }
    let kind = u16::from_le_bytes([header[16], header[17]]);
    let entry_size = u16::from_le_bytes([header[54], header[55]]);
    let entries = u16::from_le_bytes([header[56], header[57]]);
    let offset = u64::from_le_bytes(header[32..40].try_into().map_err(|_| invalid())?);
    if !matches!(kind, 2 | 3)
        || entry_size != 56
        || entries == 0
        || entries > 256
        || offset
            .checked_add(u64::from(entries) * 56)
            .is_none_or(|end| end > length)
    {
        return Err(invalid());
    }
    for entry in 0..entries {
        let mut program = [0_u8; 56];
        file.read_exact_at(&mut program, offset + u64::from(entry) * 56)
            .map_err(|_| invalid())?;
        let kind = u32::from_le_bytes(program[..4].try_into().map_err(|_| invalid())?);
        if kind == 3 {
            // A dynamic interpreter would run with setup capabilities and use
            // libraries or ld.so.preload from the target's filesystem view.
            return Err(invalid());
        }
        if kind != 2 {
            continue;
        }
        // Static PIE has PT_DYNAMIC relocations but no external dependencies.
        let start = u64::from_le_bytes(program[8..16].try_into().map_err(|_| invalid())?);
        let size = u64::from_le_bytes(program[32..40].try_into().map_err(|_| invalid())?);
        if !size.is_multiple_of(16)
            || size > 1024 * 1024
            || start.checked_add(size).is_none_or(|end| end > length)
        {
            return Err(invalid());
        }
        let mut terminated = false;
        for offset in (0..size).step_by(16) {
            let mut dynamic = [0_u8; 16];
            file.read_exact_at(&mut dynamic, start + offset)
                .map_err(|_| invalid())?;
            let tag = u64::from_le_bytes(dynamic[..8].try_into().map_err(|_| invalid())?);
            match tag {
                0 => {
                    terminated = true;
                    break;
                }
                1 => return Err(invalid()),
                _ => {}
            }
        }
        if !terminated {
            return Err(invalid());
        }
    }
    Ok(())
}

fn checked_bwrap() -> Result<&'static str> {
    let bwrap = ["/usr/bin/bwrap", "/bin/bwrap"]
        .into_iter()
        .find(|path| Path::new(path).is_file())
        .ok_or_else(|| unsupported("system bubblewrap is unavailable"))?;
    if std::fs::metadata(bwrap)?.mode() & 0o6000 != 0 {
        return Err(unsupported(
            "strict sandbox requires non-setuid bubblewrap with user namespaces",
        ));
    }
    let mut command = Command::new(bwrap);
    command.arg("--help").env_clear();
    bind_parent_lifetime(&mut command);
    let output = command.output()?;
    if !output.status.success() {
        return Err(unsupported("bubblewrap capability probe failed"));
    }
    let help = String::from_utf8_lossy(&output.stdout);
    for flag in ["--bind-fd", "--ro-bind-fd", "--cap-add", "--unshare-user"] {
        if !help.contains(flag) {
            return Err(unsupported(format!(
                "system bubblewrap lacks required option {flag}"
            )));
        }
    }
    Ok(bwrap)
}

fn capabilities() -> Result<BackendCapabilities> {
    checked_helper()?;
    checked_bwrap()?;
    // A null pathname never changes a mount; ENOSYS distinguishes an absent
    // syscall. Actual namespace permissions are still checked at installation.
    unsafe {
        libc::syscall(
            libc::SYS_mount_setattr,
            libc::AT_FDCWD,
            std::ptr::null::<libc::c_char>(),
            0,
            std::ptr::null::<MountAttr>(),
            0,
        );
    }
    let noexec = std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOSYS);
    Ok(BackendCapabilities {
        strict_filesystem: true,
        noexec,
        network_off: true,
    })
}

fn run_outer(spec: &ExecutionSpec, status: &mut File) -> Result<i32> {
    validate_stdio()?;
    let helper = checked_helper()?;
    let prepared = prepare(spec)?;
    let bwrap = checked_bwrap()?;
    if !prepared.root_writable && prepared.mounts.iter().any(|mount| mount.write) {
        // A failed diagnostic write must not reject an otherwise valid invocation.
        let _ = writeln!(
            std::io::stderr(),
            "linux-sandbox: warning: pre-existing hardlinks in writable paths may modify files outside write grants; writable paths are not scanned for hardlinks; continuing"
        );
    }
    let (mut command, preserved) = bwrap_command(spec, status, helper, prepared, bwrap)?;
    let mut keep: BTreeSet<RawFd> = preserved.iter().map(AsRawFd::as_raw_fd).collect();
    keep.insert(status.as_raw_fd());
    close_unlisted_fds(&keep)?;
    for fd in keep {
        set_cloexec(fd, false)?;
    }
    #[cfg(coverage)]
    {
        unsafe extern "C" {
            fn __llvm_profile_write_file() -> libc::c_int;
        }
        // Exec replaces this image without running the profiler's exit handler.
        if unsafe { __llvm_profile_write_file() } != 0 {
            return Err(unsupported("cannot write outer helper coverage profile"));
        }
    }
    let error = command.exec();
    Err(error.into())
}

fn bwrap_command(
    spec: &ExecutionSpec,
    status: &File,
    helper: File,
    prepared: Prepared,
    bwrap: &str,
) -> Result<(Command, Vec<File>)> {
    let mut command = Command::new(bwrap);
    command.env_clear().args([
        "--die-with-parent",
        "--unshare-user",
        "--unshare-pid",
        "--as-pid-1",
        "--unshare-ipc",
        "--unshare-uts",
        "--cap-drop",
        "ALL",
        "--cap-add",
        "CAP_SYS_ADMIN",
        "--cap-add",
        "CAP_SETPCAP",
        "--tmpfs",
        "/",
    ]);
    if spec.permissions.network.mode == NetworkMode::Off {
        command.arg("--unshare-net");
    }
    let inner = InnerLaunch {
        launch: spec.launch.clone(),
        network: spec.permissions.network.mode,
        noexec: prepared.noexec.clone(),
        mounts: mount_checks(&prepared)?,
    };
    let mut preserved = Vec::new();
    append_mounts(
        &mut command,
        prepared.mounts,
        &prepared.hidden,
        &prepared.search_dirs,
        &mut preserved,
    )?;
    for (name, target) in prepared.aliases {
        command.arg("--symlink").arg(target).arg(name);
    }
    for path in &prepared.search_dirs {
        command.args(["--remount-ro", path]);
    }
    for (path, directory) in &prepared.hidden {
        if *directory && !prepared.search_dirs.contains(path) {
            command.args(["--remount-ro", path]);
        }
    }
    // Fresh virtual process/device files do not reveal host proc or Unix sockets.
    command.args(["--proc", "/proc", "--dev", "/dev"]);
    // Exec the pinned static helper through its descriptor. A private mount
    // destination could require creating a file under an explicitly RO root.
    let helper_path = format!("/proc/self/fd/{}", helper.as_raw_fd());
    preserved.push(helper);
    if !prepared.root_writable {
        command.args(["--remount-ro", "/"]);
    }
    command.args([
        "--chdir",
        &spec.launch.cwd,
        "--",
        &helper_path,
        "--strict-inner",
    ]);
    let mut input = sealed_spec(&inner)?;
    input.seek(SeekFrom::Start(0))?;
    command
        .arg("--execution-spec-fd")
        .arg(input.as_raw_fd().to_string())
        .arg("--status-fd")
        .arg(status.as_raw_fd().to_string());
    preserved.push(input);
    Ok((command, preserved))
}

fn append_mounts(
    command: &mut Command,
    mounts: Vec<Mount>,
    hidden: &[(String, bool)],
    search_dirs: &BTreeSet<String>,
    preserved: &mut Vec<File>,
) -> Result<()> {
    let mut operations: Vec<_> = mounts
        .into_iter()
        .map(MountOperation::Bind)
        .chain(
            hidden
                .iter()
                .map(|(path, directory)| MountOperation::Hide(path.clone(), *directory)),
        )
        .chain(
            search_dirs
                .iter()
                .filter(|path| !hidden.iter().any(|(hidden, _)| hidden == *path))
                .cloned()
                .map(MountOperation::SearchDirectory),
        )
        .collect();
    operations.sort_by_key(|operation| Path::new(operation.path()).components().count());
    for operation in operations {
        match operation {
            MountOperation::Bind(mount) => {
                command
                    .arg(if mount.write {
                        "--bind-fd"
                    } else {
                        "--ro-bind-fd"
                    })
                    .arg(mount.source.as_raw_fd().to_string())
                    .arg(mount.path);
                preserved.push(mount.source);
            }
            MountOperation::Hide(path, true) => {
                // Set the mode on a fresh superblock, never on a resolved destination inode.
                command.args([
                    "--perms",
                    if search_dirs.contains(&path) {
                        "111"
                    } else {
                        "000"
                    },
                    "--tmpfs",
                    &path,
                ]);
            }
            MountOperation::Hide(path, false) => {
                let empty = File::open("/dev/null")?;
                command
                    .args(["--perms", "000", "--ro-bind-data"])
                    .arg(empty.as_raw_fd().to_string())
                    .arg(path);
                preserved.push(empty);
            }
            MountOperation::SearchDirectory(path) => {
                command.args(["--perms", "111", "--tmpfs", &path]);
            }
        }
    }
    Ok(())
}

// Only the outer helper constructs and seals this payload. The target never inherits it.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InnerLaunch {
    launch: asc_sandbox_types::LaunchSpec,
    network: NetworkMode,
    noexec: Vec<String>,
    mounts: BTreeMap<String, MountCheck>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum MountCheck {
    Bind {
        device: u64,
        inode: u64,
        readonly: bool,
    },
    Mask {
        mode: u32,
        directory: bool,
    },
}

fn mount_checks(plan: &Prepared) -> Result<BTreeMap<String, MountCheck>> {
    let mut checks = BTreeMap::new();
    if plan.search_dirs.is_empty() {
        return Ok(checks);
    }
    for mount in &plan.mounts {
        let source = mount.source.metadata()?;
        checks.insert(
            mount.path.clone(),
            MountCheck::Bind {
                device: source.dev(),
                inode: source.ino(),
                readonly: !mount.write,
            },
        );
    }
    for (path, directory) in &plan.hidden {
        checks.insert(
            path.clone(),
            MountCheck::Mask {
                mode: 0,
                directory: *directory,
            },
        );
    }
    for path in &plan.search_dirs {
        checks.insert(
            path.clone(),
            MountCheck::Mask {
                mode: 0o111,
                directory: true,
            },
        );
    }
    Ok(checks)
}

fn verify_installed_mounts(checks: &BTreeMap<String, MountCheck>) -> Result<()> {
    use rustix::fs::{
        self, AtFlags, Mode, OFlags, ResolveFlags, StatVfsMountFlags, StatxAttributes, StatxFlags,
    };
    // Each directory mask gets a fresh superblock, distinct from every bind and other mask.
    let mut devices: BTreeSet<_> = checks
        .values()
        .filter_map(|check| match check {
            MountCheck::Bind { device, .. } => Some(*device),
            MountCheck::Mask { .. } => None,
        })
        .collect();
    for (path, check) in checks {
        // Inspect one descriptor throughout, including when another process renames an ancestor.
        let fd = fs::openat2(
            fs::CWD,
            path.as_str(),
            OFlags::PATH | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::NO_SYMLINKS,
        )
        .map_err(|e| unsupported(format!("cannot verify mount at {path}: {e}")))?;
        let file = File::from(fd);
        let stat = fs::statx(&file, "", AtFlags::EMPTY_PATH, StatxFlags::BASIC_STATS)
            .map_err(std::io::Error::from)?;
        let metadata = file.metadata()?;
        let readonly = fs::fstatvfs(&file)
            .map_err(std::io::Error::from)?
            .f_flag
            .contains(StatVfsMountFlags::RDONLY);
        let matches = match check {
            MountCheck::Bind {
                device,
                inode,
                readonly: required,
            } => metadata.dev() == *device && metadata.ino() == *inode && (!required || readonly),
            MountCheck::Mask { mode, directory } => {
                readonly
                    && metadata.mode() & 0o7777 == *mode
                    && if *directory {
                        metadata.is_dir()
                            && devices.insert(metadata.dev())
                            && fs::fstatfs(&file).map_err(std::io::Error::from)?.f_type
                                == libc::TMPFS_MAGIC
                    } else {
                        metadata.is_file() && metadata.len() == 0
                    }
            }
        };
        if !matches
            || !(stat.stx_attributes_mask & stat.stx_attributes)
                .contains(StatxAttributes::MOUNT_ROOT)
        {
            return Err(unsupported(format!(
                "installed mount differs from plan at {path}"
            )));
        }
    }
    Ok(())
}

fn sealed_spec(spec: &InnerLaunch) -> Result<File> {
    let name = CString::new("asc-execution").map_err(|e| unsupported(e.to_string()))?;
    let fd =
        unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    serde_json::to_writer(&mut file, spec).map_err(|e| unsupported(e.to_string()))?;
    let seals = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
    if unsafe { libc::fcntl(fd, libc::F_ADD_SEALS, seals) } == -1 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(file)
}

fn run_inner(spec: &InnerLaunch, status: &mut File) -> Result<i32> {
    validate_stdio()?;
    set_cloexec(status.as_raw_fd(), true)?;
    // Bubblewrap's devpts setup can enter a second user namespace. Clone the
    // mount namespace so our retained CAP_SYS_ADMIN owns the noexec changes.
    if !spec.noexec.is_empty() && unsafe { libc::unshare(libc::CLONE_NEWNS) } != 0 {
        return Err(unsupported(format!(
            "cannot create noexec mount namespace: {}",
            std::io::Error::last_os_error()
        )));
    }
    for path in &spec.noexec {
        install_noexec(path)?;
    }
    // Setup capabilities are removed before any target-controlled environment,
    // argument, dynamic loader, or executable can run.
    drop_capabilities()?;
    // A target must not reopen the trusted status pipe or inspect its parent's
    // plan through /proc/<helper>/fd or /proc/<helper>/mem.
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    crate::seccomp::apply_strict_restrictions(spec.network == NetworkMode::Full)?;
    close_unlisted_fds(&BTreeSet::from([status.as_raw_fd()]))?;
    emit(status, &BackendStatus::Installed)?;
    let mut target = Command::new(&spec.launch.resolved_program);
    target
        .arg0(&spec.launch.program)
        .args(&spec.launch.args)
        .current_dir(&spec.launch.cwd)
        .env_clear()
        .envs(&spec.launch.env);
    let mut child = target.spawn()?;
    if let Err(error) = emit(status, &BackendStatus::Started) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    let outcome = wait_for_target(child.id())?;
    emit(
        status,
        &BackendStatus::Exited {
            exit_code: outcome.code(),
            signal: outcome.signal(),
        },
    )?;
    // This helper is PID 1. Its exit makes the kernel kill all remaining
    // descendants, including children that created their own process groups.
    Ok(outcome
        .code()
        .unwrap_or_else(|| 128 + outcome.signal().unwrap_or(0)))
}

fn wait_for_target(target: u32) -> Result<ExitStatus> {
    let target = i32::try_from(target).map_err(|_| unsupported("invalid target PID"))?;
    loop {
        let mut status = 0;
        let reaped = unsafe { libc::waitpid(-1, &mut status, 0) };
        if reaped == target {
            return Ok(ExitStatus::from_raw(status));
        }
        if reaped == -1 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error.into());
            }
        }
        // Reap orphaned descendants while the primary target is still active.
    }
}

#[repr(C)]
struct MountAttr {
    set: u64,
    clear: u64,
    propagation: u64,
    userns_fd: u64,
}

fn install_noexec(path: &str) -> Result<()> {
    let path = CString::new(path).map_err(|e| unsupported(e.to_string()))?;
    let attr = MountAttr {
        set: MOUNT_ATTR_NOEXEC,
        clear: 0,
        propagation: 0,
        userns_fd: 0,
    };
    let result = unsafe {
        libc::syscall(
            libc::SYS_mount_setattr,
            libc::AT_FDCWD,
            path.as_ptr(),
            AT_RECURSIVE,
            &attr as *const MountAttr,
            std::mem::size_of::<MountAttr>(),
        )
    };
    if result != 0 {
        return Err(unsupported(format!(
            "cannot install recursive noexec: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

#[repr(C)]
struct CapHeader {
    version: u32,
    pid: i32,
}
#[repr(C)]
#[derive(Default)]
struct CapData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

fn drop_capabilities() -> Result<()> {
    for cap in 0..64 {
        let supported = unsafe { libc::prctl(libc::PR_CAPBSET_READ, cap, 0, 0, 0) };
        if supported == -1 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL) {
                break;
            }
            return Err(std::io::Error::last_os_error().into());
        }
        if supported != 0 && unsafe { libc::prctl(libc::PR_CAPBSET_DROP, cap, 0, 0, 0) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    if unsafe {
        libc::prctl(
            libc::PR_CAP_AMBIENT,
            libc::PR_CAP_AMBIENT_CLEAR_ALL,
            0,
            0,
            0,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let header = CapHeader {
        version: 0x20080522,
        pid: 0,
    };
    let data = [CapData::default(), CapData::default()];
    if unsafe { libc::syscall(libc::SYS_capset, &header as *const CapHeader, data.as_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut observed = [CapData::default(), CapData::default()];
    if unsafe {
        libc::syscall(
            libc::SYS_capget,
            &header as *const CapHeader,
            observed.as_mut_ptr(),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    if observed
        .iter()
        .any(|c| c.effective != 0 || c.permitted != 0 || c.inheritable != 0)
    {
        return Err(unsupported(
            "setup capabilities remained after privilege drop",
        ));
    }
    Ok(())
}

fn validate_stdio() -> Result<()> {
    for fd in 0..=2 {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EBADF) {
                continue;
            }
            return Err(std::io::Error::last_os_error().into());
        }
        // fstat succeeded and initialized the structure.
        let stat = unsafe { stat.assume_init() };
        match stat.st_mode & libc::S_IFMT {
            libc::S_IFSOCK => {
                return Err(unsupported(
                    "socket standard I/O bypasses network isolation",
                ));
            }
            libc::S_IFDIR => {
                return Err(unsupported(
                    "directory standard I/O bypasses filesystem isolation",
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

fn set_cloexec(fd: RawFd, close: bool) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1
        || unsafe {
            libc::fcntl(
                fd,
                libc::F_SETFD,
                if close {
                    flags | libc::FD_CLOEXEC
                } else {
                    flags & !libc::FD_CLOEXEC
                },
            )
        } == -1
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

fn close_unlisted_fds(keep: &BTreeSet<RawFd>) -> Result<()> {
    let descriptors = std::fs::read_dir("/proc/self/fd")?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().parse::<RawFd>().ok()))
        .collect::<std::io::Result<Vec<_>>>()?;
    for fd in descriptors
        .into_iter()
        .flatten()
        .filter(|fd| *fd > 2 && !keep.contains(fd))
    {
        unsafe {
            libc::close(fd);
        }
    }
    Ok(())
}

fn emit(status: &mut impl Write, event: &BackendStatus) -> Result<()> {
    let mut frame = serde_json::to_vec(event).map_err(|e| unsupported(e.to_string()))?;
    frame.push(b'\n');
    if frame.len() > libc::PIPE_BUF {
        return Err(unsupported("backend status exceeds atomic pipe write size"));
    }
    // Parent death may kill this helper immediately after target exec. A whole
    // line must fit in one atomic pipe write so it cannot leave a partial JSON record.
    status.write_all(&frame)?;
    status.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    #[derive(Default)]
    struct InterruptedAfterWrite(Vec<u8>);

    impl Write for InterruptedAfterWrite {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if !self.0.is_empty() {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn status_frames_survive_termination_after_the_first_write() {
        for event in [
            BackendStatus::Installed,
            BackendStatus::Started,
            BackendStatus::Exited {
                exit_code: Some(7),
                signal: None,
            },
            BackendStatus::Failed {
                message: "cannot execute \"target\"".into(),
            },
        ] {
            let mut output = InterruptedAfterWrite::default();
            emit(&mut output, &event).unwrap();
            assert_eq!(output.0.last(), Some(&b'\n'));
            assert_eq!(
                serde_json::from_slice::<BackendStatus>(&output.0).unwrap(),
                event
            );
        }
    }

    #[test]
    fn oversized_status_never_writes_a_partial_frame() {
        let mut output = InterruptedAfterWrite::default();
        let event = BackendStatus::Failed {
            message: "x".repeat(libc::PIPE_BUF),
        };
        assert!(emit(&mut output, &event).is_err());
        assert!(output.0.is_empty());
    }

    #[test]
    fn static_helper_rejects_interpreters_dependencies_and_truncated_headers() {
        let mut bytes = vec![0_u8; 152];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[16..18].copy_from_slice(&3_u16.to_le_bytes());
        bytes[32..40].copy_from_slice(&64_u64.to_le_bytes());
        bytes[54..56].copy_from_slice(&56_u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&1_u16.to_le_bytes());
        let check = |bytes: &[u8]| {
            let mut file = tempfile::tempfile().unwrap();
            file.write_all(bytes).unwrap();
            validate_static_elf(&file)
        };
        assert!(check(&bytes).is_ok());
        bytes[64..68].copy_from_slice(&3_u32.to_le_bytes());
        assert!(check(&bytes).is_err());
        bytes[64..68].copy_from_slice(&2_u32.to_le_bytes());
        bytes[72..80].copy_from_slice(&120_u64.to_le_bytes());
        bytes[96..104].copy_from_slice(&32_u64.to_le_bytes());
        assert!(
            check(&bytes).is_ok(),
            "static PIE relocations are supported"
        );
        bytes[120..128].copy_from_slice(&1_u64.to_le_bytes());
        assert!(
            check(&bytes).is_err(),
            "DT_NEEDED requires a dynamic dependency"
        );
        bytes[120..128].copy_from_slice(&7_u64.to_le_bytes());
        bytes[136..144].copy_from_slice(&7_u64.to_le_bytes());
        assert!(check(&bytes).is_err(), "unterminated dynamic section");
        assert!(check(&bytes[..70]).is_err());
        bytes[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(check(&bytes).is_err(), "program header offset overflow");
    }

    #[test]
    fn descriptor_transfer_does_not_leak_to_parallel_children() {
        let mut input = tempfile::tempfile().unwrap();
        input.write_all(b"isolated descriptor\n").unwrap();
        input.seek(SeekFrom::Start(0)).unwrap();
        let fd = input.as_raw_fd();
        std::thread::scope(|scope| {
            let other = scope.spawn(|| {
                for _ in 0..16 {
                    assert!(
                        Command::new("/bin/sh")
                            .args(["-c", &format!("test ! -e /proc/self/fd/{fd}")])
                            .status()
                            .unwrap()
                            .success()
                    );
                }
            });
            for _ in 0..16 {
                input.seek(SeekFrom::Start(0)).unwrap();
                let mut command = Command::new("/bin/cat");
                command.arg(format!("/proc/self/fd/{fd}"));
                command.stdout(std::process::Stdio::piped());
                let output = spawn_with_fds(&mut command, &[fd])
                    .unwrap()
                    .wait_with_output()
                    .unwrap();
                assert!(output.status.success());
                assert_eq!(output.stdout, b"isolated descriptor\n");
                assert_ne!(
                    unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC,
                    0
                );
            }
            other.join().unwrap();
        });
    }

    #[test]
    fn descriptor_transfer_rejects_unprotected_parent_descriptors() {
        let input = tempfile::tempfile().unwrap();
        set_cloexec(input.as_raw_fd(), false).unwrap();
        assert!(spawn_with_fds(&mut Command::new("/bin/true"), &[input.as_raw_fd()]).is_err());
        set_cloexec(input.as_raw_fd(), true).unwrap();
        assert!(spawn_with_fds(&mut Command::new("/bin/true"), &[0]).is_err());
    }

    #[test]
    fn descriptor_spawn_rejects_departed_parent() {
        use std::io::{BufRead as _, BufReader};
        use std::process::Stdio;

        if let Some(target) = std::env::var_os("ASC_SANDBOX_PARENT_RACE_TARGET") {
            let parent = std::process::id();
            let mut command = Command::new("/usr/bin/touch");
            command.arg(target);
            // Pause the child before the lifetime hook to force the death-before-prctl race.
            unsafe {
                command.pre_exec(move || {
                    let ready = b"PREEXEC\n";
                    if libc::write(libc::STDOUT_FILENO, ready.as_ptr().cast(), ready.len()) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    while libc::getppid() as u32 == parent {
                        libc::usleep(1000);
                    }
                    Ok(())
                });
            }
            let _ = spawn_with_fds(&mut command, &[]);
            panic!("the test parent should have been killed before spawn returned");
        }
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("must-not-execute");
        let mut parent = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "runtime::tests::descriptor_spawn_rejects_departed_parent",
                "--nocapture",
            ])
            .env("ASC_SANDBOX_PARENT_RACE_TARGET", &target)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = BufReader::new(parent.stdout.take().unwrap());
        loop {
            let mut line = String::new();
            assert_ne!(
                output.read_line(&mut line).unwrap(),
                0,
                "child never reached pre-exec"
            );
            if line.contains("PREEXEC") {
                break;
            }
        }
        parent.kill().unwrap();
        parent.wait().unwrap();
        let mut remaining = Vec::new();
        output.read_to_end(&mut remaining).unwrap();
        assert!(
            !target.exists(),
            "child executed after its parent died before prctl"
        );
    }

    fn fact(path: &Path) -> PathFact {
        let path = path.canonicalize().unwrap();
        let meta = path.metadata().unwrap();
        PathFact {
            path: path.to_str().unwrap().into(),
            canonical_path: path.to_str().unwrap().into(),
            is_directory: meta.is_dir(),
            device: meta.dev(),
            inode: meta.ino(),
        }
    }

    fn fixture() -> (TempDir, ExecutionSpec) {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("program");
        std::fs::write(&program, "binary").unwrap();
        let spec: ExecutionSpec = serde_json::from_value(serde_json::json!({
            "version": 1,
            "launch": {"program":"program", "resolved_program":program, "args":[], "cwd":dir.path(), "env":{}},
            "permissions": {"version":1, "filesystem":{"default":"deny", "rules":[
                {"effect":"allow", "path":dir.path(), "rights":["read","write"]}
            ]}, "execution":{"deny_paths":[]}, "network":{"mode":"off"}},
            "path_facts": [fact(dir.path()), fact(&program)]
        })).unwrap();
        (dir, spec)
    }

    fn deny(spec: &mut ExecutionSpec, path: &Path, right: &str) {
        spec.path_facts.push(fact(path));
        spec.permissions.filesystem.rules.push(
            serde_json::from_value(serde_json::json!({
                "effect":"deny", "path":path, "rights":[right]
            }))
            .unwrap(),
        );
    }

    #[test]
    fn installed_view_rejects_changed_ancestor_before_start() {
        if std::env::var_os("ASC_SANDBOX_INTEGRATION").is_none() {
            return;
        }
        let (dir, mut spec) = fixture();
        let sessions = dir.path().join("sessions");
        let current = sessions.join("group/current");
        let tools = dir.path().join("tools");
        let data = tools.join("data");
        std::fs::create_dir_all(&current).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        deny(&mut spec, &sessions, "read");
        spec.permissions
            .filesystem
            .rules
            .last_mut()
            .unwrap()
            .excluded_paths = vec![current.to_str().unwrap().into()];
        spec.permissions.filesystem.rules.push(
            serde_json::from_value(serde_json::json!({
                "effect":"allow","path":data,"rights":["read"]
            }))
            .unwrap(),
        );
        spec.path_facts.extend([fact(&current), fact(&data)]);
        let prepared = prepare(&spec).unwrap();
        let helper = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("linux-sandbox");
        let mut status = tempfile::tempfile().unwrap();
        let (mut command, files) = bwrap_command(
            &spec,
            &status,
            File::open(helper).unwrap(),
            prepared,
            checked_bwrap().unwrap(),
        )
        .unwrap();
        assert!(!command.get_args().any(|arg| arg == "--chmod"));
        let identity = |path: &Path| {
            let meta = path.metadata().unwrap();
            (meta.dev(), meta.ino(), meta.mode())
        };
        let group = sessions.join("group");
        let paths = [&data, &sessions, &group, &current];
        let before = paths.map(|path| identity(path));
        // The symbolic name still leads to the very same approved data. Only its route changed.
        std::fs::rename(&tools, dir.path().join("saved-tools")).unwrap();
        symlink("saved-tools", &tools).unwrap();
        let mut fds: Vec<_> = files.iter().map(AsRawFd::as_raw_fd).collect();
        fds.push(status.as_raw_fd());
        command
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let output = spawn_with_fds(&mut command, &fds)
            .unwrap()
            .wait_with_output()
            .unwrap();
        status.rewind().unwrap();
        let mut frames = String::new();
        status.read_to_string(&mut frames).unwrap();
        assert_eq!(paths.map(|path| identity(path)), before);
        assert_eq!(
            output.status.code(),
            Some(125),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let event: BackendStatus = serde_json::from_str(&frames).unwrap();
        assert!(
            matches!(event, BackendStatus::Failed { message } if message.contains(&format!("cannot verify mount at {}", data.display())))
        );
    }

    #[test]
    fn denied_ancestor_cannot_be_reopened_by_deeper_allow() {
        let (dir, mut spec) = fixture();
        let secret = dir.path().join("secret");
        std::fs::create_dir(&secret).unwrap();
        let child = secret.join("key");
        std::fs::write(&child, "secret").unwrap();
        deny(&mut spec, &secret, "read");
        spec.path_facts.push(fact(&child));
        spec.permissions.filesystem.rules.push(
            serde_json::from_value(serde_json::json!({
                "effect":"allow", "path":child, "rights":["read","write"]
            }))
            .unwrap(),
        );
        let plan = prepare(&spec).unwrap();
        assert!(
            !plan
                .mounts
                .iter()
                .any(|mount| Path::new(&mount.path).starts_with(&secret))
        );
        assert!(
            plan.hidden
                .iter()
                .any(|(path, _)| Path::new(path) == secret)
        );
    }

    #[test]
    fn deny_write_overrides_nested_write_grant() {
        let (dir, mut spec) = fixture();
        let protected = dir.path().join("protected");
        std::fs::create_dir(&protected).unwrap();
        let child = protected.join("child");
        std::fs::write(&child, "content").unwrap();
        deny(&mut spec, &protected, "write");
        spec.path_facts.push(fact(&child));
        spec.permissions.filesystem.rules.push(
            serde_json::from_value(serde_json::json!({
                "effect":"allow", "path":child, "rights":["write"]
            }))
            .unwrap(),
        );
        let plan = prepare(&spec).unwrap();
        assert!(
            plan.mounts
                .iter()
                .filter(|m| Path::new(&m.path).starts_with(&protected))
                .all(|m| !m.write)
        );
    }

    #[test]
    fn exceptions_restore_inherited_grants_and_keep_nested_restrictions() {
        let (dir, mut spec) = fixture();
        let parent = dir.path().join("sessions");
        let child = parent.join("group/current");
        std::fs::create_dir_all(&child).unwrap();
        let secret = child.join("private");
        std::fs::write(&secret, "secret").unwrap();
        deny(&mut spec, &parent, "read");
        spec.permissions
            .filesystem
            .rules
            .last_mut()
            .unwrap()
            .excluded_paths = vec![child.to_str().unwrap().into()];
        spec.path_facts.push(fact(&child));
        deny(&mut spec, &secret, "read");
        spec.permissions
            .execution
            .deny_paths
            .push(parent.to_str().unwrap().into());
        let plan = prepare(&spec).unwrap();
        assert!(
            plan.mounts
                .iter()
                .any(|mount| Path::new(&mount.path) == child && mount.write)
        );
        assert!(
            plan.hidden
                .iter()
                .any(|(path, _)| Path::new(path) == secret)
        );
        assert!(plan.search_dirs.contains(parent.to_str().unwrap()));
        assert!(
            plan.search_dirs
                .contains(parent.join("group").to_str().unwrap())
        );
        assert!(plan.noexec.contains(&child.to_str().unwrap().to_owned()));
        let mut changed = spec.clone();
        changed
            .path_facts
            .iter_mut()
            .find(|fact| Path::new(&fact.path) == child)
            .unwrap()
            .inode += 1;
        assert!(prepare(&changed).is_err());
        let allowed = child.join("data");
        std::fs::write(&allowed, "public").unwrap();
        std::fs::hard_link(&allowed, child.join("alias")).unwrap();
        prepare(&spec).unwrap();
        std::fs::hard_link(&secret, child.join("secret-alias")).unwrap();
        assert!(
            prepare(&spec).is_err(),
            "independent deny must still reject hardlink aliases"
        );
    }

    #[test]
    fn rejects_search_directories_inside_launch_alias_mounts() {
        let (dir, mut spec) = fixture();
        let parent = dir.path().join("sessions");
        let outside = dir.path().join("outside");
        let nested = outside.join("nested");
        std::fs::create_dir(&parent).unwrap();
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("program"), "binary").unwrap();
        let link = parent.join("link");
        symlink(&outside, &link).unwrap();
        deny(&mut spec, &parent, "read");
        spec.launch.cwd = link.to_str().unwrap().into();
        spec.launch.resolved_program = link.join("nested/program").to_str().unwrap().into();
        for path in [&spec.launch.cwd, &spec.launch.resolved_program] {
            let mut record = fact(Path::new(path));
            record.path = path.clone();
            spec.path_facts.push(record);
        }
        let error = prepare(&spec).unwrap_err().to_string();
        assert!(error.contains("search directories require canonical mount destinations"));
        // The same aliases are safe when no hidden parent needs synthetic directories.
        spec.permissions.filesystem.rules.pop();
        prepare(&spec).unwrap();
    }

    #[test]
    fn rejects_aliases_routed_into_search_directories() {
        let (dir, mut spec) = fixture();
        let parent = dir.path().join("sessions");
        let group = parent.join("group");
        let deep = dir.path().join("deep");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&group).unwrap();
        std::fs::create_dir(&deep).unwrap();
        std::fs::create_dir_all(outside.join("nested")).unwrap();
        symlink(&group, deep.join("shortcut")).unwrap();
        symlink(&outside, group.join("link")).unwrap();
        for path in [deep.join("shortcut/link"), group.join("link/nested")] {
            spec.permissions.filesystem.rules.push(
                serde_json::from_value(serde_json::json!({
                    "effect":"allow", "path":path, "rights":["write"]
                }))
                .unwrap(),
            );
            let mut record = fact(&path);
            record.path = path.to_str().unwrap().into();
            spec.path_facts.push(record);
        }
        deny(&mut spec, &parent, "read");
        assert!(
            prepare(&spec)
                .unwrap_err()
                .to_string()
                .contains("search directories require canonical mount destinations")
        );
    }

    #[test]
    fn missing_program_permission_is_not_implicitly_granted() {
        let (_dir, mut spec) = fixture();
        spec.permissions.filesystem.rules.clear();
        assert!(prepare(&spec).is_err());
    }

    #[test]
    fn rejects_replaced_identity_before_mounting() {
        let (_dir, mut spec) = fixture();
        spec.path_facts[0].inode += 1;
        assert!(
            prepare(&spec)
                .unwrap_err()
                .to_string()
                .contains("identity changed")
        );
    }

    #[test]
    fn rejects_hardlink_and_symlink_denial_aliases() {
        let (dir, mut spec) = fixture();
        let key = dir.path().join("key");
        std::fs::write(&key, "secret").unwrap();
        std::fs::hard_link(&key, dir.path().join("alias")).unwrap();
        deny(&mut spec, &key, "read");
        assert!(prepare(&spec).unwrap_err().to_string().contains("hardlink"));
        std::fs::remove_file(dir.path().join("alias")).unwrap();
        let link = dir.path().join("link");
        symlink(&key, &link).unwrap();
        spec.permissions.filesystem.rules.last_mut().unwrap().path = link.to_str().unwrap().into();
        let mut alias_fact = fact(&key);
        alias_fact.path = link.to_str().unwrap().into();
        spec.path_facts.push(alias_fact);
        assert!(prepare(&spec).unwrap_err().to_string().contains("symlinks"));
    }

    #[test]
    fn rejects_reserved_permission_sources_through_aliases() {
        let aliases = TempDir::new().unwrap();
        for source in ["/proc", "/proc/sys", "/dev", "/dev/pts"] {
            let alias = aliases.path().join("source");
            symlink(source, &alias).unwrap();
            for path in [Path::new(source), alias.as_path()] {
                let (_dir, mut spec) = fixture();
                let mut source_fact = fact(path);
                source_fact.path = path.to_str().unwrap().into();
                spec.path_facts.push(source_fact);
                spec.permissions.filesystem.rules.push(
                    serde_json::from_value(serde_json::json!({
                        "effect":"allow", "path":path, "rights":["read"]
                    }))
                    .unwrap(),
                );
                let error = prepare(&spec).unwrap_err().to_string();
                assert!(
                    error.contains("reserved sandbox infrastructure path"),
                    "{error}"
                );
            }
            std::fs::remove_file(alias).unwrap();
        }
    }

    #[test]
    fn pins_ancestors_of_denied_paths() {
        let (dir, mut spec) = fixture();
        let parent = dir.path().join("parent");
        std::fs::create_dir(&parent).unwrap();
        let key = parent.join("key");
        std::fs::write(&key, "secret").unwrap();
        deny(&mut spec, &key, "read");
        let plan = prepare(&spec).unwrap();
        assert!(
            plan.mounts
                .iter()
                .any(|mount| Path::new(&mount.path) == parent)
        );
    }

    #[test]
    fn parent_pinning_uses_original_root_after_path_replacement() {
        let directory = TempDir::new().unwrap();
        let allowed = directory.path().join("allowed");
        let parent = allowed.join("nested/parent");
        let outside = directory.path().join("outside");
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::create_dir_all(outside.join("nested/parent")).unwrap();
        let original = parent.metadata().unwrap();
        let objects = [open_object(allowed.to_str().unwrap(), &[fact(&allowed)]).unwrap()];

        // Force the reviewed window: the rule is pinned, but its pathname now redirects outside.
        std::fs::rename(&allowed, directory.path().join("previous-allowed")).unwrap();
        symlink(&outside, &allowed).unwrap();
        let replacement = parent.metadata().unwrap();
        assert_ne!(original.ino(), replacement.ino());
        let pinned = open_parent(&parent, objects.iter())
            .unwrap()
            .metadata()
            .unwrap();
        assert_eq!(
            (pinned.dev(), pinned.ino()),
            (original.dev(), original.ino())
        );
    }

    #[test]
    fn parent_pinning_rejects_intermediate_and_final_symlinks() {
        for replaced in ["nested", "nested/parent"] {
            let directory = TempDir::new().unwrap();
            let allowed = directory.path().join("allowed");
            let parent = allowed.join("nested/parent");
            let outside = directory.path().join("outside");
            std::fs::create_dir_all(&parent).unwrap();
            std::fs::create_dir_all(outside.join("parent")).unwrap();
            let objects = [open_object(allowed.to_str().unwrap(), &[fact(&allowed)]).unwrap()];
            assert!(open_parent(&parent, objects.iter()).is_ok());
            assert!(open_parent(&allowed, objects.iter()).is_ok());
            assert!(open_parent(&outside, objects.iter()).is_err());

            let replaced = allowed.join(replaced);
            std::fs::rename(&replaced, directory.path().join("previous-parent")).unwrap();
            symlink(&outside, &replaced).unwrap();
            assert!(
                open_parent(&parent, objects.iter())
                    .unwrap_err()
                    .to_string()
                    .contains("cannot pin ancestor")
            );
        }
    }

    #[test]
    fn rejects_initial_program_inside_noexec_range() {
        let (dir, mut spec) = fixture();
        spec.permissions
            .execution
            .deny_paths
            .push(dir.path().to_str().unwrap().into());
        assert!(
            prepare(&spec)
                .unwrap_err()
                .to_string()
                .contains("initial executable")
        );
    }

    #[test]
    fn fd_protocol_rejects_alias_and_stdio_descriptors() {
        for values in [
            ["0", "4"],
            ["3", "3"],
            ["-1", "4"],
            ["x", "4"],
            ["2147483646", "2147483647"],
        ] {
            assert!(
                parse_fds(&[
                    "--execution-spec-fd".into(),
                    values[0].into(),
                    "--status-fd".into(),
                    values[1].into()
                ])
                .is_err()
            );
        }
    }
}
