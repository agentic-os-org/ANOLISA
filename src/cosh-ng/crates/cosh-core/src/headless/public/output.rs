use std::os::fd::{AsFd, OwnedFd};

use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
use tokio::time::{Duration, Instant};

use super::Cancellation;

struct OutputFd {
    file: OwnedFd,
    flags: OFlags,
}

impl Drop for OutputFd {
    fn drop(&mut self) {
        let _ = fcntl_setfl(&self.file, self.flags);
    }
}

pub(super) async fn write(
    stream: impl AsFd,
    mut bytes: &[u8],
    deadline: Instant,
    cancellation: &mut Cancellation,
) -> std::io::Result<()> {
    let file = rustix::io::dup(stream)?;
    let flags = fcntl_getfl(&file)?;
    let output = OutputFd { file, flags };
    fcntl_setfl(&output.file, flags | OFlags::NONBLOCK)?;
    while !bytes.is_empty() {
        if Instant::now() >= deadline {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        match rustix::io::write(&output.file, &bytes[..bytes.len().min(4096)]) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(written) => bytes = &bytes[written..],
            Err(rustix::io::Errno::INTR) => continue,
            Err(rustix::io::Errno::AGAIN) => {
                tokio::select! {
                    _ = cancellation.wait() => return Err(std::io::ErrorKind::Interrupted.into()),
                    _ = tokio::time::sleep_until(deadline.min(Instant::now() + Duration::from_millis(10))) => {},
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
