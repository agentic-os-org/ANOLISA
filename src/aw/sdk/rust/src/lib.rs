//! One application client for component-owned security and compression APIs.
//!
//! Calls return results for the application to adopt. They do not enter AW
//! Service, authorize native tools, install components or manage system daemons.

#[cfg(feature = "sec-core")]
pub mod sec_core;
#[cfg(feature = "tokenless")]
pub mod tokenless;

/// Correlation shared across component calls, never an authorization credential.
#[derive(Debug, Clone)]
pub struct CallContext {
    /// Stable application or Agent identifier.
    pub agent_id: String,
    /// Conversation identifier; also the sec-core compatibility trace label.
    pub session_id: Option<String>,
    /// Tool-call identifier; also the sec-core compatibility invocation label.
    pub tool_use_id: Option<String>,
    /// Optional W3C carrier, separate from compatibility labels.
    #[cfg(feature = "sec-core")]
    pub trace_context: Option<sec_core::TraceCarrierV1>,
}

impl CallContext {
    /// Starts application attribution without inventing session or tool-call IDs.
    pub fn new(agent_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            session_id: None,
            tool_use_id: None,
            #[cfg(feature = "sec-core")]
            trace_context: None,
        }
    }
}

/// Explicit endpoints and state for the components used by an application.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Existing daemon socket; no service is started during construction.
    #[cfg(feature = "sec-core")]
    pub sec_core_socket: Option<std::path::PathBuf>,
    /// Runtime settings, persistent state and optional managed RTK executable.
    #[cfg(feature = "tokenless")]
    pub tokenless: Option<tokenless::Config>,
}

/// SDK call failures retain their component's error information.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The application did not configure this component.
    #[error("{0} is not configured")]
    NotConfigured(&'static str),
    /// Configuration does not identify explicit local resources.
    #[error("invalid SDK configuration: {0}")]
    Configuration(&'static str),
    /// Security transport or response validation failed.
    #[cfg(feature = "sec-core")]
    #[error(transparent)]
    SecCore(#[from] sec_core::ScanError),
    /// Compression, recovery or its runtime dependency failed.
    #[cfg(feature = "tokenless")]
    #[error(transparent)]
    Tokenless(#[from] tokenless_runtime::RuntimeError),
}

/// Reusable application facade; components retain independent behavior and state.
pub struct Client {
    #[cfg(feature = "sec-core")]
    sec_core: Option<sec_core::Client>,
    #[cfg(feature = "tokenless")]
    tokenless: Option<tokenless::Client>,
}

impl Client {
    /// Opens configured component clients without installing or launching services.
    ///
    /// Tokenless may create its configured state directory and SQLite databases.
    /// Dropping the client releases handles but preserves persistent state.
    ///
    /// # Errors
    /// Returns invalid configuration or Tokenless state initialization failures.
    pub fn new(config: Config) -> Result<Self, Error> {
        #[cfg(not(any(feature = "sec-core", feature = "tokenless")))]
        let _ = config;
        Ok(Self {
            #[cfg(feature = "sec-core")]
            sec_core: config
                .sec_core_socket
                .map(sec_core::Client::new)
                .transpose()?,
            #[cfg(feature = "tokenless")]
            tokenless: config.tokenless.map(tokenless::Client::new).transpose()?,
        })
    }

    /// Accesses the configured security client.
    ///
    /// # Errors
    /// Returns [`Error::NotConfigured`] when no daemon socket was supplied.
    #[cfg(feature = "sec-core")]
    pub fn sec_core(&self) -> Result<&sec_core::Client, Error> {
        self.sec_core
            .as_ref()
            .ok_or(Error::NotConfigured("sec-core"))
    }

    /// Accesses the configured Tokenless runtime.
    ///
    /// # Errors
    /// Returns [`Error::NotConfigured`] when no runtime configuration was supplied.
    #[cfg(feature = "tokenless")]
    pub fn tokenless(&self) -> Result<&tokenless::Client, Error> {
        self.tokenless
            .as_ref()
            .ok_or(Error::NotConfigured("Tokenless"))
    }
}
