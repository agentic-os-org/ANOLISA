//! Component Runtime access with explicit state and marker-authorized recovery.

use std::path::PathBuf;

pub use tokenless_protocol::{
    AppliedOperation, BeforeModelCapabilities, BeforeModelRequest, BeforeModelResponse,
    ContentOrigin, ContentType, Disposition, OutputOptimization, PostToolCapabilities,
    PostToolRequest, PostToolResponse, PreToolAction, PreToolCapabilities, PreToolRequest,
    PreToolResponse, ProtocolError, Recoverability, RecoveryMethod, RecoveryToolName, ResultKind,
    RetrieveRequest, RetrieveResponse, ToolResultStatus,
};
pub use tokenless_runtime::{RuntimeConfig, RuntimeError};

use crate::{CallContext, Error};

/// Component settings without PATH lookup or ambient state-directory selection.
#[derive(Debug, Clone)]
pub struct Config {
    /// Existing component settings; `data_dir` must be an explicit absolute path.
    pub runtime: RuntimeConfig,
    /// Installed RTK executable for PreTool. No fallback to the user's PATH.
    pub rtk_path: Option<PathBuf>,
}

impl Config {
    /// Uses component defaults with an explicit persistent state directory.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            runtime: RuntimeConfig {
                data_dir: Some(data_dir.into()),
                ..RuntimeConfig::default()
            },
            rtk_path: None,
        }
    }
}

/// Stateful component facade; dropping it preserves the caller's stored data.
pub struct Client {
    runtime: tokenless_runtime::TokenlessRuntime,
    rtk_path: Option<PathBuf>,
}

impl Client {
    pub(crate) fn new(config: Config) -> Result<Self, Error> {
        if !config
            .runtime
            .data_dir
            .as_ref()
            .is_some_and(|p| p.is_absolute())
        {
            return Err(Error::Configuration("Tokenless data_dir must be absolute"));
        }
        if config.rtk_path.as_ref().is_some_and(|p| !p.is_absolute()) {
            return Err(Error::Configuration("RTK path must be absolute"));
        }
        Ok(Self {
            runtime: tokenless_runtime::TokenlessRuntime::new(config.runtime)?,
            rtk_path: config.rtk_path,
        })
    }

    /// Transforms model-bound declarations using component recovery rules.
    ///
    /// Declaring a recovery method promises the application has registered a
    /// reachable implementation; the SDK does not register one.
    ///
    /// # Errors
    /// Returns invalid declarations, conflicting recovery tools or state failures.
    pub fn before_model(
        &self,
        request: &BeforeModelRequest,
        context: &CallContext,
    ) -> Result<BeforeModelResponse, Error> {
        Ok(self.runtime.before_model(request, &attribution(context))?)
    }

    /// Computes an RTK rewrite for the application to adopt and authorize.
    ///
    /// Uses the component's bounded RTK invocation. This API does not implement
    /// the AW Provider's block-and-retry or any native permission decision.
    ///
    /// # Errors
    /// Returns missing RTK, spawn, timeout or invalid RTK response failures.
    pub fn pre_tool(
        &self,
        request: &PreToolRequest,
        context: &CallContext,
    ) -> Result<PreToolResponse, Error> {
        let rtk = self.rtk_path.as_ref().ok_or(RuntimeError::RtkUnavailable)?;
        Ok(self.runtime.pre_tool(request, &attribution(context), rtk)?)
    }

    /// Processes a completed tool result without changing its native envelope.
    ///
    /// # Errors
    /// Returns component pipeline or state failures.
    pub fn post_tool(
        &self,
        request: &PostToolRequest,
        context: &CallContext,
    ) -> Result<PostToolResponse, Error> {
        Ok(self.runtime.post_tool(request, &attribution(context))?)
    }

    /// Retrieves only entries authorized by trusted, currently visible markers.
    ///
    /// The application supplies visibility from its conversation state; it must
    /// not accept a model's assertion that a marker is visible.
    ///
    /// # Errors
    /// Returns invalid, unauthorized, unavailable or missing stash errors.
    pub fn retrieve(
        &self,
        request: &RetrieveRequest,
        context: &CallContext,
    ) -> Result<RetrieveResponse, Error> {
        Ok(self
            .runtime
            .retrieve_authorized(request, &attribution(context))?)
    }

    /// Reports unavailable stash storage, which prevents reversible transforms.
    pub fn stash_error(&self) -> Option<&str> {
        self.runtime.stash_error()
    }

    /// Reports unavailable statistics storage without claiming recorded savings.
    pub fn stats_error(&self) -> Option<&str> {
        self.runtime.stats_error()
    }
}

fn attribution(context: &CallContext) -> tokenless_protocol::Attribution {
    tokenless_protocol::Attribution {
        agent_id: context.agent_id.clone(),
        session_id: context.session_id.clone(),
        tool_use_id: context.tool_use_id.clone(),
    }
}
