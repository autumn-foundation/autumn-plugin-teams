//! Shared test helpers.
#![allow(clippy::unwrap_used, clippy::expect_used)] // Test helpers fail loudly.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use autumn_plugin_teams::TeamsPlugin;
use autumn_plugin_teams::activity::Activity;
use autumn_plugin_teams::config::TeamsConfig;
use autumn_plugin_teams::testing::{self, TestAuthority};
use autumn_plugin_teams::transport::MemoryTransport;
use autumn_web::reexports::chrono::{DateTime, TimeZone, Utc};
use autumn_web::test::{TestClient, TestResponse};
use autumn_web::time::FixedClock;

#[allow(unused_imports)] // Not every test binary uses all of them.
pub use autumn_plugin_teams::testing::{APP_ID, CONVERSATION_ID, SERVICE_URL, TENANT_ID};

pub const PASSWORD: &str = "test-app-password-000";
pub const PATH: &str = "/teams/messages";

/// Fixed test time: 2026-01-01T00:00:00Z.
pub fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
}

pub fn now_secs() -> u64 {
    u64::try_from(now().timestamp()).unwrap()
}

pub fn authority() -> TestAuthority {
    TestAuthority::new(APP_ID)
}

/// Test config. The invoke window is wide, so in-time tests do not depend
/// on CPU load. Slow-path tests use [`slow_config`] and a [`Gate`].
pub fn config() -> TeamsConfig {
    let mut c = TeamsConfig::default();
    c.invoke_timeout_ms = 4_000;
    c.api.initial_backoff_ms = 1;
    c
}

/// Config with a short invoke window, for slow-path tests.
pub fn slow_config() -> TeamsConfig {
    let mut c = config();
    c.invoke_timeout_ms = 50;
    c
}

/// A closed gate. A handler waits on it; the test opens it. No timing.
#[derive(Clone, Default)]
pub struct Gate(Arc<tokio::sync::Notify>);

impl Gate {
    pub async fn wait(&self) {
        self.0.notified().await;
    }

    /// Opens the gate. A later `wait` also passes.
    pub fn open(&self) {
        self.0.notify_one();
    }
}

/// Fails if `fut` does not end in 10 s. Use it where a hang is the bug.
pub async fn within<F: std::future::Future>(fut: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(10), fut)
        .await
        .expect("did not end in 10 s")
}

/// A plugin with test credentials, the test key, a fixed clock, and this
/// fake transport.
pub fn plugin(t: &MemoryTransport) -> TeamsPlugin {
    plugin_with(t, config())
}

pub fn plugin_with(t: &MemoryTransport, c: TeamsConfig) -> TeamsPlugin {
    TeamsPlugin::with_config(c)
        .with_app_id(APP_ID)
        .with_app_password(PASSWORD)
        .with_transport(t.clone())
        .with_clock(Arc::new(FixedClock::at(now())))
        .with_key_source(authority().key_source())
}

/// Posts `body` with this `Authorization` value.
pub async fn post_raw(client: &TestClient, auth: Option<&str>, body: &str) -> TestResponse {
    let mut req = client.post(PATH).header("content-type", "application/json");
    if let Some(a) = auth {
        req = req.header("authorization", a);
    }
    req.body(body.to_owned()).send().await
}

/// Posts `activity` with a valid token from the test authority.
pub async fn post(client: &TestClient, activity: &Activity) -> TestResponse {
    let auth = authority().bearer(activity, now_secs());
    post_raw(
        client,
        Some(&auth),
        &serde_json::to_string(activity).unwrap(),
    )
    .await
}

/// Posts `activity` with a token for these claims.
pub async fn post_claims(
    client: &TestClient,
    activity: &Activity,
    claims: &serde_json::Value,
) -> TestResponse {
    let auth = format!("Bearer {}", authority().sign(claims));
    post_raw(
        client,
        Some(&auth),
        &serde_json::to_string(activity).unwrap(),
    )
    .await
}

/// Valid claims for the test activity.
pub fn claims() -> serde_json::Value {
    authority().claims(now_secs(), SERVICE_URL)
}

pub fn message(text: &str) -> Activity {
    testing::message(text)
}

/// Waits for one value, or fails after 5 s.
pub async fn recv<T>(rx: &mut tokio::sync::mpsc::UnboundedReceiver<T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out")
        .expect("channel closed")
}

/// Waits until `f` is true, or fails after `limit`.
pub async fn wait_until(limit: Duration, f: &(impl Fn() -> bool + Sync)) {
    let start = tokio::time::Instant::now();
    while !f() {
        assert!(start.elapsed() < limit, "condition not met in {limit:?}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Requests to the Connector (not the token endpoint).
pub fn connector_requests(
    t: &MemoryTransport,
) -> Vec<autumn_plugin_teams::transport::RecordedRequest> {
    t.requests()
        .into_iter()
        .filter(|r| r.url.starts_with(SERVICE_URL))
        .collect()
}
