//! One-open, one-write management calls and explicit bootstrap acknowledgements.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// Native operations isolated so partial writes and kernel errno can be reproduced.
pub trait SecurityFs: Send + Sync {
    /// Reads a control node from offset zero, bounded to one MiB.
    /// # Errors
    /// Returns filesystem errors or oversized responses.
    fn read(&self, node: &str) -> io::Result<Vec<u8>>;
    /// Makes exactly one write; a short write is never continued.
    /// # Errors
    /// Returns the original syscall error, preserving errno.
    fn write(&self, node: &str, payload: &[u8]) -> io::Result<usize>;
    /// Opens a private command file; EALREADY is the template's acknowledgement.
    /// # Errors
    /// Returns other errors or missing acknowledgement without confirming deployment.
    fn bootstrap(&self, path: &Path) -> io::Result<()>;
}

/// Direct access to the running host's Lua-LSM instance.
#[derive(Debug, Clone)]
pub struct NativeSecurityFs {
    directory: PathBuf,
}

impl Default for NativeSecurityFs {
    fn default() -> Self {
        Self::new("/sys/kernel/security/lua")
    }
}

impl NativeSecurityFs {
    /// Selects the securityfs directory; it is never inferred from a policy.
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }
}

impl SecurityFs for NativeSecurityFs {
    fn read(&self, node: &str) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        File::open(self.directory.join(node))?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 1024 * 1024 {
            return Err(io::Error::other("oversized Lua-LSM response"));
        }
        Ok(bytes)
    }
    fn write(&self, node: &str, payload: &[u8]) -> io::Result<usize> {
        OpenOptions::new()
            .write(true)
            .open(self.directory.join(node))?
            .write(payload)
    }
    fn bootstrap(&self, path: &Path) -> io::Result<()> {
        match File::open(path) {
            Err(error)
                if error.raw_os_error() == Some(rustix::io::Errno::ALREADY.raw_os_error()) =>
            {
                Ok(())
            }
            Err(error) => Err(error),
            Ok(_) => Err(io::Error::other("Lua-LSM bootstrap was not acknowledged")),
        }
    }
}
