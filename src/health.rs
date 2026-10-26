//! Health indicator for `/actuator/health`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use autumn_web::actuator::{HealthCheckOutput, HealthIndicator, HealthStatus, IndicatorGroup};
use futures::future::BoxFuture;
use serde_json::json;
use tokio::time::Instant;

use crate::client::TeamsClient;
use crate::error::TeamsError;

/// Time limit for one check. 1.5 s is less than the 2 s autumn indicator
/// timeout.
const CHECK_TIMEOUT: Duration = Duration::from_millis(1_500);
/// Largest time to keep a `Down` result. After a short outage, a new check
/// runs soon.
pub const DOWN_TTL: Duration = Duration::from_secs(5);

/// A stored result and its expiry.
#[derive(Clone)]
struct Entry {
    stored: Instant,
    until: Instant,
    out: HealthCheckOutput,
}

/// Checks that the bot can get an app token.
///
/// - `Up`: the token endpoint gives a token (or a cached token is valid).
/// - `Down`: the token request fails.
/// - `Unknown`: before start, or with no app password.
///
/// It keeps `Up` for `[teams.health] cache_secs` and `Down` for 5 s or less.
/// One check runs at a time. Details give a short error code only.
pub struct TeamsHealth {
    client: OnceLock<TeamsClient>,
    readiness: bool,
    cache: Mutex<Option<Entry>>,
    cache_for: OnceLock<Duration>,
    /// Lets one check run at a time.
    refresh: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for TeamsHealth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeamsHealth")
            .field("started", &self.client.get().is_some())
            .field("readiness", &self.readiness)
            .finish_non_exhaustive()
    }
}

impl TeamsHealth {
    pub(crate) const fn new(readiness: bool) -> Self {
        Self {
            client: OnceLock::new(),
            readiness,
            cache: Mutex::new(None),
            cache_for: OnceLock::new(),
            refresh: tokio::sync::Mutex::const_new(()),
        }
    }

    /// Sets the client once. Later calls do nothing.
    pub(crate) fn set(&self, client: TeamsClient, cache_for: Duration) {
        let _ = self.cache_for.set(cache_for);
        let _ = self.client.set(client);
    }

    fn entry(&self) -> Option<Entry> {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn cached(&self) -> Option<HealthCheckOutput> {
        let entry = self.entry()?;
        (Instant::now() < entry.until).then_some(entry.out)
    }

    /// A result stored at or after `since`, whatever its TTL. A probe that
    /// waited for another check uses that check's result.
    fn stored_since(&self, since: Instant) -> Option<HealthCheckOutput> {
        let entry = self.entry()?;
        (entry.stored >= since).then_some(entry.out)
    }

    fn store(&self, out: &HealthCheckOutput) {
        let up_ttl = self.cache_for.get().copied().unwrap_or_default();
        let ttl = if out.status == HealthStatus::Up {
            up_ttl
        } else {
            up_ttl.min(DOWN_TTL)
        };
        let now = Instant::now();
        *self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Entry {
            stored: now,
            until: now + ttl,
            out: out.clone(),
        });
    }

    async fn run_check(client: &TeamsClient) -> HealthCheckOutput {
        let down = |code: String| {
            HealthCheckOutput::down()
                .with_details(HashMap::from([("error".to_owned(), json!(code))]))
        };
        match tokio::time::timeout(CHECK_TIMEOUT, client.check_credentials()).await {
            Ok(Ok(())) => HealthCheckOutput::up(),
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "teams health check failed");
                down(error_code(&e))
            }
            Err(_) => down("timeout".to_owned()),
        }
    }
}

fn unknown(reason: &str) -> HealthCheckOutput {
    HealthCheckOutput {
        status: HealthStatus::Unknown,
        details: HashMap::from([("reason".to_owned(), json!(reason))]),
    }
}

/// A short error code for public health output.
fn error_code(err: &TeamsError) -> String {
    match err {
        // OAuth codes are short snake_case words. Keep only those characters.
        TeamsError::Token { code } => code
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(64)
            .collect(),
        TeamsError::RateLimited { .. } => "rate_limited".to_owned(),
        TeamsError::Api { status, .. } => format!("http_{status}"),
        TeamsError::Transport(_) => "network".to_owned(),
        _ => "error".to_owned(),
    }
}

impl HealthIndicator for TeamsHealth {
    fn check(&self) -> BoxFuture<'_, HealthCheckOutput> {
        Box::pin(async move {
            let Some(client) = self.client.get() else {
                return unknown("not started");
            };
            if !client.has_credentials() {
                return unknown("no app password");
            }
            if let Some(out) = self.cached() {
                return out;
            }
            // Single flight: a probe that waits here gets the new result,
            // also with `cache_secs = 0`.
            let since = Instant::now();
            let _turn = self.refresh.lock().await;
            if let Some(out) = self.cached().or_else(|| self.stored_since(since)) {
                return out;
            }
            let out = Self::run_check(client).await;
            self.store(&out);
            out
        })
    }

    fn group(&self) -> IndicatorGroup {
        if self.readiness {
            IndicatorGroup::Readiness
        } else {
            IndicatorGroup::HealthOnly
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unknown_before_start() {
        let h = TeamsHealth::new(false);
        let out = h.check().await;
        assert_eq!(out.status, HealthStatus::Unknown);
        assert_eq!(out.details["reason"], json!("not started"));
        assert!(format!("{h:?}").contains("started: false"));
    }

    #[test]
    fn error_codes_are_short() {
        assert_eq!(
            error_code(&TeamsError::Token {
                code: "invalid_client".into()
            }),
            "invalid_client"
        );
        assert_eq!(
            error_code(&TeamsError::Token {
                code: "bad code <script>".into()
            }),
            "badcodescript"
        );
        assert_eq!(
            error_code(&TeamsError::RateLimited {
                retry_after_secs: None
            }),
            "rate_limited"
        );
        assert_eq!(error_code(&TeamsError::Transport("dns".into())), "network");
        assert_eq!(error_code(&TeamsError::Decode("x".into())), "error");
    }
}
