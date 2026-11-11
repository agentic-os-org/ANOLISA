mod enumeration;
pub(super) use enumeration::{local_accounts, nss_accounts};

use std::ffi::{CStr, CString};
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

use super::{Error, Result, COSH_PATH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Account {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: String,
    pub shell: String,
}

pub(super) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-$".contains(&b))
}

pub(super) fn supported_bash(path: &str) -> bool {
    matches!(path, "/bin/bash" | "/usr/bin/bash")
}

pub(super) fn match_local(nss: Account, passwd: &str) -> Result<Account> {
    let mut found = None;
    for local in enumeration::parse_local_accounts(passwd)? {
        if local.name != nss.name && local.uid != nss.uid {
            continue;
        }
        if local != nss || found.is_some() {
            return Err(Error::new(
                "NSS and local account identity do not match uniquely",
            ));
        }
        found = Some(local);
    }
    found.ok_or_else(|| Error::new("account is not a matching local account"))
}

pub(super) fn require_platform() -> Result<()> {
    if !cfg!(target_os = "linux") {
        return Err(Error::new(
            "login management is supported only on Linux RPM systems",
        ));
    }
    let release = String::from_utf8(read_trusted(Path::new("/etc/os-release"), 64 * 1024)?)
        .map_err(|_| Error::new("invalid operating system identity"))?;
    validate_platform(&release)?;
    Ok(())
}

pub(super) fn lookup(name: Option<&str>, euid: u32) -> Result<Account> {
    require_platform()?;
    let account = lookup_nss(name, euid)?;
    let passwd = String::from_utf8(read_trusted(Path::new("/etc/passwd"), 1024 * 1024)?)
        .map_err(|_| Error::new("invalid local account encoding"))?;
    match_local(account, &passwd)
}

pub(super) fn lookup_nss(name: Option<&str>, euid: u32) -> Result<Account> {
    if name.is_some_and(|name| !valid_name(name)) {
        return Err(Error::usage());
    }
    let name = name
        .map(CString::new)
        .transpose()
        .map_err(|_| Error::usage())?;
    let mut entry = std::mem::MaybeUninit::<nix::libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0u8; 64 * 1024];
    let status = unsafe {
        match &name {
            Some(name) => nix::libc::getpwnam_r(
                name.as_ptr(),
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            ),
            None => nix::libc::getpwuid_r(
                euid,
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            ),
        }
    };
    if status != 0 || result.is_null() {
        return Err(Error::new("unable to resolve account identity"));
    }
    let entry = unsafe { entry.assume_init() };
    let account = account_from_entry(&entry)?;
    if !valid_name(&account.name) {
        return Err(Error::new("unsupported account name"));
    }
    Ok(account)
}

fn account_from_entry(entry: &nix::libc::passwd) -> Result<Account> {
    let text = |pointer: *const nix::libc::c_char| -> Result<String> {
        if pointer.is_null() {
            return Err(Error::new("incomplete NSS account identity"));
        }
        unsafe { CStr::from_ptr(pointer) }
            .to_str()
            .map(str::to_owned)
            .map_err(|_| Error::new("non-UTF-8 account fields are unsupported"))
    };
    let account = Account {
        name: text(entry.pw_name)?,
        uid: entry.pw_uid,
        gid: entry.pw_gid,
        home: text(entry.pw_dir)?,
        shell: text(entry.pw_shell)?,
    };
    Ok(account)
}

pub(super) fn trusted_path(path: &Path, executable: bool) -> Result<PathBuf> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(Error::new("untrusted system path"));
    }
    let canonical = resolve_trusted_path(path, &mut 16, executable)?;
    let metadata = fs::metadata(&canonical).map_err(Error::io)?;
    if !metadata.is_file() {
        return Err(Error::new("invalid trusted system file"));
    }
    if executable {
        validate_public_execution(metadata.mode(), false, has_access_acl(&canonical)?)?;
    }
    Ok(canonical)
}

fn resolve_trusted_path(
    path: &Path,
    remaining_links: &mut usize,
    public_execution: bool,
) -> Result<PathBuf> {
    let mut current = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir => current = PathBuf::from("/"),
            Component::CurDir => continue,
            Component::ParentDir => {
                current.pop();
            }
            Component::Normal(name) => current.push(name),
            _ => return Err(Error::new("unsupported system path")),
        }
        let metadata = fs::symlink_metadata(&current).map_err(Error::io)?;
        if metadata.uid() != 0
            || (!metadata.file_type().is_symlink() && metadata.mode() & 0o022 != 0)
        {
            return Err(Error::new("system path is not administrator-controlled"));
        }
        if metadata.is_dir() && public_execution {
            validate_public_execution(metadata.mode(), true, has_access_acl(&current)?)?;
        }
        if metadata.file_type().is_symlink() {
            if *remaining_links == 0 {
                return Err(Error::new("system path has too many symbolic links"));
            }
            *remaining_links -= 1;
            let target = fs::read_link(&current).map_err(Error::io)?;
            let target = if target.is_absolute() {
                target
            } else {
                current.parent().unwrap().join(target)
            };
            current = resolve_trusted_path(&target, remaining_links, public_execution)?;
        }
    }
    Ok(current)
}

pub(super) fn validate_platform(release: &str) -> Result<()> {
    let mut id = None;
    let mut version = None;
    for line in release.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let slot = match key {
            "ID" => &mut id,
            "VERSION_ID" => &mut version,
            _ => continue,
        };
        if slot.is_some() {
            return Err(Error::new("ambiguous operating system identity"));
        }
        *slot = Some(value.trim_matches('"'));
    }
    if id != Some("alinux")
        || !version.is_some_and(|version| version == "4" || version.starts_with("4."))
    {
        return Err(Error::new(
            "login management supports Alibaba Cloud Linux 4 RPM installations only",
        ));
    }
    Ok(())
}

pub(super) fn read_trusted(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let canonical = trusted_path(path, false)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(canonical)
        .map_err(Error::io)?;
    let metadata = file.metadata().map_err(Error::io)?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.nlink() != 1
    {
        return Err(Error::new("untrusted system file"));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(Error::io)?;
    if bytes.len() > limit {
        return Err(Error::new("system file exceeds size limit"));
    }
    Ok(bytes)
}

pub(super) fn command(program: &str) -> Result<Command> {
    let path = trusted_path(Path::new(program), true)?;
    let mut command = Command::new(path);
    command
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LANG", "C")
        .env("LC_ALL", "C");
    Ok(command)
}

pub(super) fn verify_package(path: &str, package: &str) -> Result<PathBuf> {
    let actual = trusted_path(Path::new(path), true)?;
    if fs::metadata(&actual).map_err(Error::io)?.mode() & 0o6000 != 0 && path != "/usr/bin/sudo" {
        return Err(Error::new("setuid/setgid shell programs are unsupported"));
    }
    for checked in [Path::new(path), actual.as_path()] {
        let output = command("/usr/bin/rpm")?
            .args(["-q", "--qf", "%{NAME}\n", "-f"])
            .arg(checked)
            .stdin(Stdio::null())
            .output()
            .map_err(Error::io)?;
        if !output.status.success() || output.stdout != format!("{package}\n").as_bytes() {
            return Err(Error::new(
                "system executable does not belong to the supported package",
            ));
        }
    }
    Ok(actual)
}

pub(super) fn verify_shells(action: super::Action, bash: &str) -> Result<String> {
    verify_mutation_with(
        action,
        bash,
        verify_bash,
        || {
            let cosh = verify_package(COSH_PATH, "cosh-ng")?;
            verified_elf_file(&cosh)?;
            Ok(())
        },
        || {
            String::from_utf8(read_trusted(Path::new("/etc/shells"), 64 * 1024)?)
                .map_err(|_| Error::new("invalid /etc/shells"))
        },
    )
}

pub(super) fn verify_mutation_with(
    action: super::Action,
    bash: &str,
    verify_bash: impl FnOnce(&str) -> Result<String>,
    verify_cosh: impl FnOnce() -> Result<()>,
    read_shells: impl FnOnce() -> Result<String>,
) -> Result<String> {
    let hash = verify_bash(bash)?;
    if action == super::Action::Set {
        verify_cosh()?;
    }
    let shells = read_shells()?;
    let required = if action == super::Action::Set {
        vec![bash, COSH_PATH]
    } else {
        vec![bash]
    };
    for expected in required {
        if !shells
            .lines()
            .any(|line| line.split('#').next().unwrap_or("").trim() == expected)
        {
            return Err(Error::new(
                "supported shell path is absent from /etc/shells",
            ));
        }
    }
    Ok(hash)
}

pub(super) fn verify_bash(bash: &str) -> Result<String> {
    if !supported_bash(bash) {
        return Err(Error::new("unsupported Bash recovery path"));
    }
    let actual = verify_package(bash, "bash")?;
    let mut file = verified_elf_file(&actual)?;
    let mut hash = Sha256::new();
    hash.update(b"\x7fELF");
    let mut buffer = [0u8; 16384];
    loop {
        let count = file.read(&mut buffer).map_err(Error::io)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn verified_elf_file(path: &Path) -> Result<File> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(path)
        .map_err(Error::io)?;
    let metadata = file.metadata().map_err(Error::io)?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o6022 != 0
        || metadata.mode() & 0o555 != 0o555
    {
        return Err(Error::new("untrusted shell executable"));
    }
    validate_public_execution(metadata.mode(), false, has_file_access_acl(&file)?)?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).map_err(Error::io)?;
    if magic != *b"\x7fELF" {
        return Err(Error::new(
            "supported shell entry must be a compiled ELF executable",
        ));
    }
    Ok(file)
}

pub(super) fn verify_account_tools() -> Result<()> {
    for program in ["/usr/sbin/vipw", "/usr/sbin/usermod"] {
        let actual = trusted_path(Path::new(program), true)?;
        verified_elf_file(&actual)?;
        for checked in [Path::new(program), actual.as_path()] {
            let output = command("/usr/bin/rpm")?
                .args([
                    "-q",
                    "--qf",
                    "%{NAME} %{VERSION}-%{RELEASE} %{ARCH}\n",
                    "-f",
                ])
                .arg(checked)
                .stdin(Stdio::null())
                .output()
                .map_err(Error::io)?;
            if !output.status.success()
                || !account_tool_package_matches(program, &output.stdout, std::env::consts::ARCH)
            {
                return Err(Error::new("native account editing requires the verified ALinux4 shadow-4.14.3-2.alnx4 tool build for this architecture"));
            }
        }
    }
    Ok(())
}

pub(super) fn validate_public_execution(
    mode: u32,
    directory: bool,
    has_access_acl: bool,
) -> Result<()> {
    let required = if directory { 0o111 } else { 0o555 };
    if has_access_acl || mode & required != required {
        return Err(Error::new(
            "managed shell assets must be publicly accessible without extended access ACLs",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn has_access_acl(path: &Path) -> Result<bool> {
    use std::os::unix::ffi::OsStrExt;
    let path =
        CString::new(path.as_os_str().as_bytes()).map_err(|_| Error::new("invalid system path"))?;
    let count = unsafe {
        nix::libc::getxattr(
            path.as_ptr(),
            c"system.posix_acl_access".as_ptr(),
            std::ptr::null_mut(),
            0,
        )
    };
    access_acl_result(count)
}

#[cfg(target_os = "linux")]
fn has_file_access_acl(file: &File) -> Result<bool> {
    use std::os::fd::AsRawFd;
    let count = unsafe {
        nix::libc::fgetxattr(
            file.as_raw_fd(),
            c"system.posix_acl_access".as_ptr(),
            std::ptr::null_mut(),
            0,
        )
    };
    access_acl_result(count)
}

#[cfg(target_os = "linux")]
fn access_acl_result(count: nix::libc::ssize_t) -> Result<bool> {
    if count >= 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(nix::libc::ENODATA)
        || error.raw_os_error() == Some(nix::libc::EOPNOTSUPP)
    {
        return Ok(false);
    }
    Err(Error::io(error))
}

#[cfg(not(target_os = "linux"))]
fn has_access_acl(_path: &Path) -> Result<bool> {
    Err(Error::new(
        "managed shell access ACL validation is supported only on Linux",
    ))
}

#[cfg(not(target_os = "linux"))]
fn has_file_access_acl(_file: &File) -> Result<bool> {
    Err(Error::new(
        "managed shell access ACL validation is supported only on Linux",
    ))
}

#[cfg(all(test, target_os = "linux"))]
mod access_tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn extended_acl_denial_is_detected_even_with_public_mode_bits() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bash");
        fs::write(&path, b"\x7fELFfixture").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let mut acl = 2u32.to_le_bytes().to_vec();
        for (tag, perm, id) in [
            (1u16, 7u16, u32::MAX),
            (2, 0, 1001),
            (4, 5, u32::MAX),
            (16, 5, u32::MAX),
            (32, 5, u32::MAX),
        ] {
            acl.extend_from_slice(&tag.to_le_bytes());
            acl.extend_from_slice(&perm.to_le_bytes());
            acl.extend_from_slice(&id.to_le_bytes());
        }
        let cpath = CString::new(path.as_os_str().as_bytes()).unwrap();
        let result = unsafe {
            nix::libc::setxattr(
                cpath.as_ptr(),
                c"system.posix_acl_access".as_ptr(),
                acl.as_ptr().cast(),
                acl.len(),
                0,
            )
        };
        assert_eq!(
            result,
            0,
            "fixture access ACL: {}",
            std::io::Error::last_os_error()
        );
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o755);
        assert!(has_access_acl(&path).unwrap());
        assert!(validate_public_execution(0o755, false, has_access_acl(&path).unwrap()).is_err());
        let file = File::open(&path).unwrap();
        assert!(has_file_access_acl(&file).unwrap());
    }
}

pub(super) fn account_tool_package_matches(program: &str, reported: &[u8], arch: &str) -> bool {
    matches!(program, "/usr/sbin/usermod" | "/usr/sbin/vipw")
        && matches!(arch, "x86_64" | "aarch64")
        && reported == format!("shadow 4.14.3-2.alnx4 {arch}\n").as_bytes()
}
