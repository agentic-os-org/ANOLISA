use super::MAX_BYTES;
use std::time::Instant;

pub(super) fn read_frame(fd: std::os::fd::RawFd, deadline: Instant) -> Result<Vec<u8>, ()> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 1024];
    loop {
        let remaining = deadline.checked_duration_since(Instant::now()).ok_or(())?;
        let mut descriptor = nix::libc::pollfd {
            fd,
            events: nix::libc::POLLIN,
            revents: 0,
        };
        let timeout_ms = remaining.as_millis().clamp(1, 500) as i32;
        let ready = unsafe { nix::libc::poll(&mut descriptor, 1, timeout_ms) };
        if ready < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        if ready <= 0 || descriptor.revents & (nix::libc::POLLERR | nix::libc::POLLNVAL) != 0 {
            return Err(());
        }
        let count = unsafe { nix::libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        if count < 0 {
            return Err(());
        }
        if count == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&buffer[..count as usize]);
        if bytes.len() > MAX_BYTES {
            return Err(());
        }
    }
}

pub(in crate::runtime) fn write_frame(
    fd: std::os::fd::RawFd,
    bytes: &[u8],
    deadline: Instant,
) -> Result<(), ()> {
    use std::os::fd::AsRawFd;

    let output = independent_output(fd)?;
    let fd = output.as_ref().map(AsRawFd::as_raw_fd).unwrap_or(fd);
    let mut metadata = std::mem::MaybeUninit::<nix::libc::stat>::uninit();
    if unsafe { nix::libc::fstat(fd, metadata.as_mut_ptr()) } != 0 {
        return Err(());
    }
    let socket =
        unsafe { metadata.assume_init() }.st_mode & nix::libc::S_IFMT == nix::libc::S_IFSOCK;
    (|| {
        let mut written = 0;
        while written < bytes.len() {
            let remaining = deadline.checked_duration_since(Instant::now()).ok_or(())?;
            let mut descriptor = nix::libc::pollfd {
                fd,
                events: nix::libc::POLLOUT,
                revents: 0,
            };
            let ready = unsafe {
                nix::libc::poll(
                    &mut descriptor,
                    1,
                    remaining.as_millis().clamp(1, 500) as i32,
                )
            };
            if ready < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            if ready <= 0
                || descriptor.revents
                    & (nix::libc::POLLERR | nix::libc::POLLNVAL | nix::libc::POLLHUP)
                    != 0
            {
                return Err(());
            }
            let count = unsafe {
                if socket {
                    nix::libc::send(
                        fd,
                        bytes[written..].as_ptr().cast(),
                        bytes.len() - written,
                        nix::libc::MSG_DONTWAIT,
                    )
                } else {
                    nix::libc::write(fd, bytes[written..].as_ptr().cast(), bytes.len() - written)
                }
            };
            if count < 0 {
                match std::io::Error::last_os_error().kind() {
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock => continue,
                    _ => return Err(()),
                }
            }
            if count == 0 {
                return Err(());
            }
            written += count as usize;
        }
        Ok(())
    })()
}

fn independent_output(fd: std::os::fd::RawFd) -> Result<Option<std::fs::File>, ()> {
    if unsafe { nix::libc::isatty(fd) } == 1 {
        return independent_terminal(fd);
    }
    let mut source = std::mem::MaybeUninit::<nix::libc::stat>::uninit();
    if unsafe { nix::libc::fstat(fd, source.as_mut_ptr()) } != 0 {
        return Err(());
    }
    let source = unsafe { source.assume_init() };
    if source.st_mode & nix::libc::S_IFMT != nix::libc::S_IFIFO {
        return Ok(None);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let flags = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFL) };
        if flags < 0 || flags & nix::libc::O_ACCMODE == nix::libc::O_RDONLY {
            return Err(());
        }
        // Reopening procfs creates a distinct description; dup would share O_NONBLOCK.
        let output = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
            .open(format!("/proc/self/fd/{fd}"))
            .map_err(|_| ())?;
        let target = output.metadata().map_err(|_| ())?;
        if target.mode() & nix::libc::S_IFMT != nix::libc::S_IFIFO
            || target.dev() != source.st_dev
            || target.ino() != source.st_ino
        {
            return Err(());
        }
        Ok(Some(output))
    }
    #[cfg(not(target_os = "linux"))]
    Err(())
}

fn independent_terminal(fd: std::os::fd::RawFd) -> Result<Option<std::fs::File>, ()> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;

    if unsafe { nix::libc::isatty(fd) } != 1 {
        return Ok(None);
    }
    let flags = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFL) };
    if flags < 0 || flags & nix::libc::O_ACCMODE == nix::libc::O_RDONLY {
        return Err(());
    }
    let mut source = std::mem::MaybeUninit::<nix::libc::stat>::uninit();
    if unsafe { nix::libc::fstat(fd, source.as_mut_ptr()) } != 0 {
        return Err(());
    }
    let source = unsafe { source.assume_init() };
    let mut name = [0u8; 4096];
    if unsafe { nix::libc::ttyname_r(fd, name.as_mut_ptr().cast(), name.len()) } != 0 {
        return Err(());
    }
    let length = name.iter().position(|byte| *byte == 0).ok_or(())?;
    let terminal = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(
            nix::libc::O_NONBLOCK
                | nix::libc::O_NOCTTY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW,
        )
        .open(std::ffi::OsStr::from_bytes(&name[..length]))
        .map_err(|_| ())?;
    let mut target = std::mem::MaybeUninit::<nix::libc::stat>::uninit();
    if unsafe { nix::libc::fstat(terminal.as_raw_fd(), target.as_mut_ptr()) } != 0 {
        return Err(());
    }
    let target = unsafe { target.assume_init() };
    if target.st_mode & nix::libc::S_IFMT != nix::libc::S_IFCHR
        || target.st_dev != source.st_dev
        || target.st_ino != source.st_ino
        || target.st_rdev != source.st_rdev
    {
        return Err(());
    }
    Ok(Some(terminal))
}
