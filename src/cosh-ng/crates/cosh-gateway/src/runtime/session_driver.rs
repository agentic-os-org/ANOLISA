//! Responsive single-owner ACP session orchestration over supervised stdio.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use thiserror::Error;

use super::{
    AcpV1BridgeError, AcpV1BridgeRead, AcpV1ClientConfig, AcpV1Observation,
    AcpV1PermissionDecision, AcpV1RequestId, AcpV1RuntimeBridge, ProcessTerminal,
    RuntimeLaunchSpec,
};

const CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(10);
const COMMAND_CAPACITY: usize = 8;
const CONTROL_CAPACITY: usize = 1;
const EVENT_CAPACITY: usize = 32;
const DEFAULT_EVENT_BYTE_BUDGET: usize = 4 * 1024 * 1024;
const MAX_TERMINAL_DETAIL_BYTES: usize = 4 * 1024;
const DEFAULT_INITIALIZE_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(70);
const SHUTDOWN_SETTLEMENT_MARGIN: Duration = Duration::from_secs(1);

// Driver and actor phases share channel/state invariants in one private
// namespace, so the layout split does not widen actor internals.
include!("session_driver/model.rs");
include!("session_driver/driver.rs");
include!("session_driver/actor.rs");

#[cfg(test)]
#[path = "session_driver/tests.rs"]
mod tests;
