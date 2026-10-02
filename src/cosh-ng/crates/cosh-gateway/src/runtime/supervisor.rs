//! Single-owner lifecycle for one local Agent Runtime child process.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use wait_timeout::ChildExt;

#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;

use super::bounded_io::{
    BoundedLineChannel, BoundedLineError, BoundedLineRead, BoundedWriteChannel, StderrCollector,
    StderrSnapshot,
};
use super::pinned_local::{PinnedDirectory, PinnedExecutable};
use super::process_group::{PlatformProcessGroup, ProcessGroupLifecycle};

const MAX_STDERR_CAPACITY: usize = 1024 * 1024;
const MAX_STDOUT_LINE_BYTES: usize = 1024 * 1024;
const MAX_ENVIRONMENT_ENTRIES: usize = 256;
const MAX_ENVIRONMENT_VALUE_BYTES: usize = 64 * 1024;
const MAX_STDIN_WRITE_TIMEOUT: Duration = Duration::from_secs(60);

// Launch validation and process ownership share pinned-resource internals; the
// fragments remain one private namespace rather than widening those details.
include!("supervisor/launch.rs");
include!("supervisor/process.rs");

#[cfg(test)]
mod tests;
