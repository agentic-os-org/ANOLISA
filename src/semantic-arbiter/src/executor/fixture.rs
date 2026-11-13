//! Scripted executors and claim storage with no external side effects.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::composition::DirectiveCapability;
use crate::executor::{
    ClaimResult, ClaimStore, EffectState, ExecutionRequest, Executor, ExecutorFailure,
};

/// Process-local claim store for deterministic tests and examples.
#[derive(Debug, Default)]
pub struct InMemoryClaimStore {
    claims: Mutex<BTreeSet<String>>,
}

impl ClaimStore for InMemoryClaimStore {
    fn claim(&self, idempotency_key: &str) -> Result<ClaimResult, ExecutorFailure> {
        let mut claims = self
            .claims
            .lock()
            .map_err(|_| ExecutorFailure::ClaimFailed)?;
        if claims.insert(idempotency_key.into()) {
            Ok(ClaimResult::New)
        } else {
            Ok(ClaimResult::Existing)
        }
    }
}

/// Scripted executor that records lifecycle calls without performing an action.
#[derive(Debug)]
pub struct ScriptedExecutor {
    capability: DirectiveCapability,
    effect: Mutex<EffectState>,
    dispatches: AtomicUsize,
    queries: AtomicUsize,
}

impl ScriptedExecutor {
    /// Create a fixture with a selected independently reported effect.
    pub fn new(capability: DirectiveCapability, effect: EffectState) -> Self {
        Self {
            capability,
            effect: Mutex::new(effect),
            dispatches: AtomicUsize::new(0),
            queries: AtomicUsize::new(0),
        }
    }

    /// Change the next query result without modifying claim state.
    pub fn set_effect(&self, effect: EffectState) -> Result<(), ExecutorFailure> {
        *self
            .effect
            .lock()
            .map_err(|_| ExecutorFailure::QueryFailed)? = effect;
        Ok(())
    }

    /// Number of dispatch calls observed by this fixture.
    pub fn dispatch_count(&self) -> usize {
        self.dispatches.load(Ordering::SeqCst)
    }

    /// Number of effect queries observed by this fixture.
    pub fn query_count(&self) -> usize {
        self.queries.load(Ordering::SeqCst)
    }
}

impl Executor for ScriptedExecutor {
    fn capability(&self) -> DirectiveCapability {
        self.capability
    }

    fn dispatch<'a>(
        &'a self,
        _request: &'a ExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecutorFailure>> + Send + 'a>> {
        Box::pin(async move {
            self.dispatches.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }

    fn query<'a>(
        &'a self,
        _request: &'a ExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<EffectState, ExecutorFailure>> + Send + 'a>> {
        Box::pin(async move {
            self.queries.fetch_add(1, Ordering::SeqCst);
            self.effect
                .lock()
                .map(|effect| *effect)
                .map_err(|_| ExecutorFailure::QueryFailed)
        })
    }
}
