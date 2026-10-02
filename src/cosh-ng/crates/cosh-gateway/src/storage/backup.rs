//! Verified online backup and new-path restore for Gateway SQLite state.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use cosh_gateway_contracts::ids::InstallationId;
use rusqlite::backup::{Backup, StepResult};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

use super::{schema, sqlite, SqliteTaskStore, StoreError};

const BACKUP_PAGES_PER_STEP: i32 = 128;
const BACKUP_RETRY_LIMIT: u32 = 100;
const BACKUP_RETRY_PAUSE: Duration = Duration::from_millis(5);
const TEMPORARY_CREATE_ATTEMPTS: u32 = 64;

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

// Backup orchestration and file hardening share private publication helpers;
// the fragments separate concerns without widening those helpers.
include!("backup/operations.rs");
include!("backup/files.rs");

#[cfg(test)]
mod tests;
