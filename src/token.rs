//! Outbound app token: OAuth 2.0 client credentials.

use std::sync::{Arc, Mutex};

use autumn_web::time::ClockSource;

use autumn_web::time::clock_unix_secs;
use serde::Deserialize;

use crate::error::TeamsError;
use crate::metrics::TeamsMetrics;
use crate::policy::{
    Outcome, RetryRule, Step, TOKEN_REFRESH_MARGIN_SECS, decide, retry_after_ms, token_expires_at,
    token_needs_refresh,
};
use crate::transport::{HttpReply, HttpRequest, HttpTransport, Method, TransportError};

/// Credentials of the bot.
#[derive(Clone)]
pub(crate) struct Credentials {
    pub app_id: String,
    pub password: Option<String>,
    /// Env var of the password, for error text.
    pub password_env: String,
    /// Full token endpoint URL.
    pub token_url: String,
    pub scope: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("app_id", &self.app_id)
            .field("has_password", &self.password.is_some())
            .field("token_url", &self.token_url)
            .finish_non_exhaustive()
    }
}

/// A cached token, its expiry (Unix seconds), and its refresh margin.
struct Cached {
    token: String,
    expires_at: u64,
    margin: u64,
}

/// The token endpoint reply.
#[derive(Deserialize)]
struct TokenReply {
    access_token: String,
    expires_in: Option<u64>,
}

/// Gets and caches the app token. One request runs at a time.
pub(crate) struct TokenProvider {
    transport: Arc<dyn HttpTransport>,
    credentials: Credentials,
    clock: Arc<dyn ClockSource>,
    metrics: Arc<TeamsMetrics>,
    rule: RetryRule,
    cache: Mutex<Option<Cached>>,
    refresh: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for TokenProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenProvider")
            .field("credentials", &self.credentials)
            .finish_non_exhaustive()
    }
}

impl TokenProvider {
    pub(crate) fn new(
        transport: Arc<dyn HttpTransport>,
        credentials: Credentials,
        clock: Arc<dyn ClockSource>,
        metrics: Arc<TeamsMetrics>,
        rule: RetryRule,
    ) -> Self {
        Self {
            transport,
            credentials,
            clock,
            metrics,
            rule,
            cache: Mutex::new(None),
            refresh: tokio::sync::Mutex::const_new(()),
        }
    }

    /// Returns `true` when an app password is set.
    pub(crate) const fn has_password(&self) -> bool {
        self.credentials.password.is_some()
    }

    /// Returns a valid token: from the cache, or a new one.
    pub(crate) async fn token(&self) -> Result<String, TeamsError> {
        if let Some(t) = self.cached() {
            return Ok(t);
        }
        // Single flight: a caller that waits here gets the new token.
        let _turn = self.refresh.lock().await;
        if let Some(t) = self.cached() {
            return Ok(t);
        }
        let result = self.request().await;
        self.metrics
            .token_request(if result.is_ok() { "ok" } else { "error" });
        let (token, expires_in) = result?;
        let now = clock_unix_secs(self.clock.as_ref());
        // A short-lived token keeps half its life, not the full margin.
        let margin = TOKEN_REFRESH_MARGIN_SECS.min(expires_in / 2);
        *self.lock() = Some(Cached {
            token: token.clone(),
            expires_at: token_expires_at(now, expires_in),
            margin,
        });
        Ok(token)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Cached>> {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The cached token, if it is not in its refresh margin.
    fn cached(&self) -> Option<String> {
        let now = clock_unix_secs(self.clock.as_ref());
        self.lock()
            .as_ref()
            .filter(|c| !token_needs_refresh(now, c.expires_at, c.margin))
            .map(|c| c.token.clone())
    }

    /// Requests a token. Retries 429, 5xx, and network errors: a token
    /// request has no side effect.
    async fn request(&self) -> Result<(String, u64), TeamsError> {
        let c = &self.credentials;
        let Some(password) = c.password.as_deref() else {
            return Err(TeamsError::Config(format!(
                "no app password; set the env var {}",
                c.password_env
            )));
        };
        let body = serde_urlencoded::to_string([
            ("grant_type", "client_credentials"),
            ("client_id", c.app_id.as_str()),
            ("client_secret", password),
            ("scope", c.scope.as_str()),
        ])
        .map_err(|e| TeamsError::Config(format!("token form: {e}")))?
        .into_bytes();
        let mut attempt: u32 = 1;
        loop {
            let req = HttpRequest::new(Method::Post, c.token_url.as_str())
                .header("content-type", "application/x-www-form-urlencoded")
                .body(body.clone());
            let result = self.transport.send(req).await;
            let (outcome, retry_after) = classify(&result);
            match decide(
                outcome,
                attempt,
                true,
                retry_after.map(retry_after_ms),
                true,
                &self.rule,
            ) {
                Step::Retry(wait_ms) => {
                    tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
                    attempt += 1;
                }
                Step::Done | Step::GiveUp | Step::RefreshToken => return finish(result),
            }
        }
    }

    /// Drops the cached token if it is `stale`. The next call gets a new one.
    pub(crate) fn invalidate(&self, stale: &str) {
        let mut cache = self.lock();
        if cache.as_ref().is_some_and(|c| c.token == stale) {
            *cache = None;
        }
    }
}

/// The policy class of an attempt, and its `Retry-After`.
pub(crate) const fn classify(result: &Result<HttpReply, TransportError>) -> (Outcome, Option<u64>) {
    let Ok(r) = result else {
        return (Outcome::NetworkError, None);
    };
    let outcome = match r.status {
        200..=299 => Outcome::Success,
        401 => Outcome::Unauthorized,
        429 => Outcome::RateLimited,
        500..=599 => Outcome::ServerError,
        _ => Outcome::ClientError,
    };
    (outcome, r.retry_after_secs)
}

/// A short OAuth error code: ASCII letters, digits, and `_` only.
fn short_code(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(64)
        .collect()
}

/// Turns the last attempt into a token or an error. The error has a code
/// only: `error_description` can echo input.
fn finish(result: Result<HttpReply, TransportError>) -> Result<(String, u64), TeamsError> {
    let token_err = |code: String| TeamsError::Token { code };
    let reply = result.map_err(|_| token_err("network".to_owned()))?;
    match reply.status {
        200..=299 => {
            let r: TokenReply =
                serde_json::from_slice(&reply.body).map_err(|_| token_err("decode".to_owned()))?;
            Ok((r.access_token, r.expires_in.unwrap_or(600)))
        }
        429 => Err(token_err("rate_limited".to_owned())),
        status => {
            let code = serde_json::from_slice::<serde_json::Value>(&reply.body)
                .ok()
                .and_then(|v| v.get("error")?.as_str().map(short_code))
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| format!("http_{status}"));
            Err(token_err(code))
        }
    }
}
