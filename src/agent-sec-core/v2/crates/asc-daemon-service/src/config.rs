use std::time::Duration;

/// Explicit transport limits supplied by the future daemon composition root.
///
/// This type intentionally has no `Default`: limits and timeouts are part of the
/// externally reviewed process/protocol configuration rather than library-owned
/// product defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceConfig {
    /// Maximum request wire-frame bytes, including an LF delimiter when present.
    pub max_request_frame_bytes: usize,
    /// Maximum response wire-frame bytes, including the LF delimiter.
    pub max_response_frame_bytes: usize,
    /// Maximum requests admitted for concurrent frame read or dispatch.
    pub max_connections: usize,
    /// Maximum overload/shutdown rejection responses served concurrently.
    pub max_rejection_connections: usize,
    /// Maximum time allowed for protocol-only transport rejection encoding.
    pub rejection_encode_timeout: Duration,
    /// Whole-frame deadline starting immediately after accept.
    pub request_read_timeout: Duration,
    /// Maximum time the transport waits for one application dispatch.
    ///
    /// Expiration releases the connection permit and requests cooperative
    /// cancellation. It cannot forcibly stop an already running blocking call.
    pub dispatch_timeout: Duration,
    /// Whole-response write deadline.
    pub response_write_timeout: Duration,
    /// Maximum graceful wait for admitted connection tasks during shutdown.
    pub drain_timeout: Duration,
    /// Backoff after an accept error before the listener is retried.
    pub accept_error_backoff: Duration,
}

impl ServiceConfig {
    /// Validates that all resource bounds are usable and non-zero.
    ///
    /// # Errors
    /// Returns a stable configuration error for a zero bound or timeout.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.max_request_frame_bytes == 0 {
            return Err(ConfigError::ZeroRequestFrameLimit);
        }
        if self.max_response_frame_bytes < 2 {
            return Err(ConfigError::ResponseFrameLimitTooSmall);
        }
        if self.max_connections == 0 {
            return Err(ConfigError::ZeroConnectionLimit);
        }
        if self.max_rejection_connections == 0 {
            return Err(ConfigError::ZeroRejectionLimit);
        }
        if self.rejection_encode_timeout.is_zero() {
            return Err(ConfigError::ZeroRejectionEncodeTimeout);
        }
        if self.request_read_timeout.is_zero() {
            return Err(ConfigError::ZeroReadTimeout);
        }
        if self.dispatch_timeout.is_zero() {
            return Err(ConfigError::ZeroDispatchTimeout);
        }
        if self.response_write_timeout.is_zero() {
            return Err(ConfigError::ZeroWriteTimeout);
        }
        if self.drain_timeout.is_zero() {
            return Err(ConfigError::ZeroDrainTimeout);
        }
        if self.accept_error_backoff.is_zero() {
            return Err(ConfigError::ZeroAcceptBackoff);
        }
        Ok(())
    }
}

/// Invalid service-framework configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// The request frame limit cannot admit any bytes.
    #[error("request frame limit must be positive")]
    ZeroRequestFrameLimit,
    /// A response needs at least one payload byte and one LF delimiter.
    #[error("response frame limit must be at least two bytes")]
    ResponseFrameLimitTooSmall,
    /// No normal connection could be admitted.
    #[error("connection limit must be positive")]
    ZeroConnectionLimit,
    /// No bounded rejection response could be admitted.
    #[error("rejection connection limit must be positive")]
    ZeroRejectionLimit,
    /// A zero rejection deadline could never encode a transport error.
    #[error("rejection encoding timeout must be positive")]
    ZeroRejectionEncodeTimeout,
    /// A zero read deadline would reject every connection.
    #[error("request read timeout must be positive")]
    ZeroReadTimeout,
    /// A zero dispatch deadline would reject every complete request.
    #[error("request dispatch timeout must be positive")]
    ZeroDispatchTimeout,
    /// A zero write deadline would reject every response.
    #[error("response write timeout must be positive")]
    ZeroWriteTimeout,
    /// A zero drain deadline would never allow graceful completion.
    #[error("drain timeout must be positive")]
    ZeroDrainTimeout,
    /// A zero accept backoff could spin on repeated listener errors.
    #[error("accept error backoff must be positive")]
    ZeroAcceptBackoff,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_config() -> ServiceConfig {
        ServiceConfig {
            max_request_frame_bytes: 1024,
            max_response_frame_bytes: 1024,
            max_connections: 4,
            max_rejection_connections: 2,
            rejection_encode_timeout: Duration::from_millis(250),
            request_read_timeout: Duration::from_secs(1),
            dispatch_timeout: Duration::from_secs(1),
            response_write_timeout: Duration::from_secs(1),
            drain_timeout: Duration::from_secs(1),
            accept_error_backoff: Duration::from_millis(10),
        }
    }

    #[test]
    fn explicit_config_rejects_zero_resource_bounds() {
        let mut config = valid_config();
        config.max_connections = 0;
        assert_eq!(config.validate(), Err(ConfigError::ZeroConnectionLimit));

        config = valid_config();
        config.rejection_encode_timeout = Duration::ZERO;
        assert_eq!(
            config.validate(),
            Err(ConfigError::ZeroRejectionEncodeTimeout)
        );

        config = valid_config();
        config.request_read_timeout = Duration::ZERO;
        assert_eq!(config.validate(), Err(ConfigError::ZeroReadTimeout));

        config = valid_config();
        config.dispatch_timeout = Duration::ZERO;
        assert_eq!(config.validate(), Err(ConfigError::ZeroDispatchTimeout));
    }
}
