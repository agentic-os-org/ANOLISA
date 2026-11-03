//! Payload integrity metadata and bounded streaming digests.

use crate::store::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

/// Integrity metadata for an owned regular file.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Payload {
    /// Lowercase SHA-256 digest.
    pub sha256: String,
    /// Unix permission bits, restricted to executable or read-only data modes.
    pub mode: u32,
}

/// Stream a file's SHA-256 without loading the payload into memory.
pub fn digest(path: &Path) -> Result<String> {
    let mut source = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
