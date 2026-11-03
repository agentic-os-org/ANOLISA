//! Trusted runtime ports; implementations retain their native authority.

use serde_json::Value;

/// A Host-owned receipt and separately delivered capability output.
#[derive(Debug, Clone)]
pub struct ProviderResult {
    /// Must bind exactly to the admitted invocation.
    pub receipt: Value,
    /// Candidate or inspection, never an adoption assertion.
    pub output: Option<Value>,
}

/// Transport failure before the Host can return a valid terminal receipt.
#[derive(Debug, thiserror::Error)]
#[error("provider host failed: {code}")]
pub struct HostError {
    /// Non-sensitive diagnostic code; provider stderr must not be embedded.
    pub code: String,
}

/// Trusted provider registry and invocation boundary.
///
/// The Host authenticates descriptors, resolves native protocols and enforces
/// execution time/output budgets. Core validates returned facts but cannot
/// preempt a synchronous implementation. Drivers are registered by the Host,
/// never dynamically loaded from untrusted descriptor strings.
pub trait ProviderHost {
    /// Returns the currently admitted descriptor for this exact provider ID.
    fn descriptor(&self, provider_id: &str) -> Option<&Value>;

    /// Executes once and returns a Host-authored receipt.
    ///
    /// # Errors
    /// Return a transport error only when no trustworthy receipt is available.
    /// Normal provider failures should have a failed receipt and no output.
    fn invoke(&mut self, invocation: &Value) -> Result<ProviderResult, HostError>;
}

/// Trusted wall clock shared with the local Host's receipt timestamps.
pub trait Clock {
    /// Returns Unix epoch milliseconds without exposing an Agent-controlled clock.
    fn now_ms(&self) -> u64;
}

/// Cooperative cancellation sampled before each provider dispatch.
pub trait Cancellation {
    /// True stops further dispatch; it does not interrupt a running Host call.
    fn is_cancelled(&self) -> bool;
}

/// Execution without a cancellation request.
pub struct NeverCancel;

impl Cancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Journal storage failure. An error must never be converted into success.
#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    /// An event is already reserved, complete or interrupted; never retry blindly.
    #[error("event is already claimed")]
    AlreadyClaimed,
    /// Storage or synchronization failed.
    #[error("journal I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// Stored chain or caller record violates the private journal format.
    #[error("invalid journal record")]
    InvalidRecord,
}

/// Atomic event reservation and acknowledged append-only execution records.
///
/// A persistent implementation reserves across processes/restarts and returns
/// evidence only after durable writes. Core never deletes reservations after
/// failure. Records contain digests and receipts, not raw capability content.
pub trait Journal {
    /// Atomically reserves a digest-keyed event and records its pinned plan.
    ///
    /// # Errors
    /// Reject an existing claim, including an interrupted run or changed plan.
    /// Returns a common/v1 evidence object only after acknowledging the claim.
    /// On error, release resources acquired by this attempt without disturbing
    /// any existing writer or deleting a reservation already made durable.
    fn claim(&mut self, event_key: &str, plan: &Value) -> Result<Value, JournalError>;

    /// Appends one private runtime record and returns common/v1 evidence.
    ///
    /// # Errors
    /// Reject unowned claims, invalid records or writes not acknowledged by storage.
    fn append(&mut self, event_key: &str, record: &Value) -> Result<Value, JournalError>;

    /// Releases local write ownership without deleting the durable reservation.
    ///
    /// Must not panic, perform fallible storage work, or permit another claim for
    /// this event. Unknown or already released keys are a no-op. Core calls this
    /// after every successful claim, including errors and unwinding; direct
    /// Journal callers must release their claims or drop the implementation.
    fn release(&mut self, event_key: &str);
}
