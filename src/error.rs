//! Error types.

use crate::auth::AuthError;

/// Error from a handler. Any error type converts with `?`.
pub type HandlerError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Result of a handler.
pub type HandlerResult<T> = Result<T, HandlerError>;

/// Plugin and client errors. Messages never hold a token or a secret.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TeamsError {
    /// Bad config or bad input from the app.
    #[error("teams config: {0}")]
    Config(String),
    /// The Connector replied with an error status.
    #[error("teams {operation} failed: http {status} {code}")]
    Api {
        /// Client operation, for example `send_to_conversation`.
        operation: &'static str,
        /// HTTP status.
        status: u16,
        /// Error code from the `ErrorResponse`, or `unknown`.
        code: String,
        /// Error message from the `ErrorResponse`, if any.
        message: Option<String>,
    },
    /// The Connector replied 429 and the client stopped.
    #[error("teams rate limited (retry after {retry_after_secs:?} s)")]
    RateLimited {
        /// The `Retry-After` value, if any.
        retry_after_secs: Option<u64>,
    },
    /// The token endpoint did not give a token.
    #[error("teams token request failed: {code}")]
    Token {
        /// The OAuth error code, for example `invalid_client`, or an HTTP or
        /// network code.
        code: String,
    },
    /// No HTTP reply: connect error or timeout.
    #[error("teams transport: {0}")]
    Transport(String),
    /// The reply is not the expected JSON.
    #[error("teams reply decode: {0}")]
    Decode(String),
    /// The `serviceUrl` is not an allowed Connector URL.
    #[error("serviceUrl is not an allowed Connector URL")]
    BadServiceUrl,
    /// The JWT check failed.
    #[error(transparent)]
    Auth(#[from] AuthError),
}

impl TeamsError {
    /// Returns the Connector error code for an [`TeamsError::Api`] error.
    #[must_use]
    pub fn api_code(&self) -> Option<&str> {
        match self {
            Self::Api { code, .. } => Some(code),
            _ => None,
        }
    }
}
