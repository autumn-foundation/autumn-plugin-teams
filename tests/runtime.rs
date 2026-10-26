//! Plugin install, startup checks, keys, health, metrics, and drain.
#![allow(clippy::unwrap_used, clippy::expect_used)] // Test helpers fail loudly.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use autumn_plugin_teams::activity::Activity;
use autumn_plugin_teams::auth::{KeySet, KeySource, OpenIdKeySource, StaticKeys};
use autumn_plugin_teams::testing::{self, TestAuthority};
use autumn_plugin_teams::transport::{HttpReply, MemoryTransport};
use autumn_plugin_teams::{TeamsError, TeamsPlugin, TeamsRuntime};
use autumn_web::actuator::{HealthIndicator, HealthStatus, IndicatorGroup, MetricsSource};
use autumn_web::config::AutumnConfig;
use autumn_web::test::TestApp;
use autumn_web::time::TickingClock;
use autumn_web::{AppState, ProcessRole};
use common::*;
use futures::future::BoxFuture;
use serde_json::json;

fn state_with(cfg: AutumnConfig) -> AppState {
    let s = AppState::for_test();
    s.insert_extension(cfg);
    s
}

/// Posts `activity` to the runtime router with a token signed by `who` at
/// `at`. Returns the status.
async fn send_as(rt: &TeamsRuntime, who: &TestAuthority, activity: &Activity, at: u64) -> u16 {
    use autumn_web::reexports::axum::body::Body;
    use autumn_web::reexports::http::Request;
    use tower::ServiceExt as _;
    let req = Request::post("/messages")
        .header("authorization", who.bearer(activity, at))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(activity).unwrap()))
        .unwrap();
    rt.router().oneshot(req).await.unwrap().status().as_u16()
}

async fn send(rt: &TeamsRuntime, activity: &Activity) -> u16 {
    send_as(rt, &authority(), activity, now_secs()).await
}

/// A key source that counts loads and can fail.
#[derive(Clone)]
struct CountingKeys {
    keys: Arc<std::sync::Mutex<KeySet>>,
    loads: Arc<AtomicU32>,
    fail: Arc<AtomicBool>,
}

impl CountingKeys {
    fn new(keys: KeySet) -> Self {
        Self {
            keys: Arc::new(std::sync::Mutex::new(keys)),
            loads: Arc::default(),
            fail: Arc::default(),
        }
    }
}

impl KeySource for CountingKeys {
    fn fetch(&self) -> BoxFuture<'_, Result<KeySet, TeamsError>> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        let out = if self.fail.load(Ordering::SeqCst) {
            Err(TeamsError::Transport("down".into()))
        } else {
            Ok(self.keys.lock().unwrap().clone())
        };
        Box::pin(async move { out })
    }
}

fn keys_of(a: &TestAuthority) -> KeySet {
    KeySet::new(vec![a.signing_key()])
}

// ----------------------------------------------------------------- install (AC1)

#[tokio::test(flavor = "multi_thread")]
async fn plugin_installs_with_one_call() {
    let t = MemoryTransport::new();
    let client = TestApp::new().plugin(plugin(&t)).build();
    assert!(autumn_plugin_teams::TeamsClient::from_state(client.state()).is_some());
    let body = client.get("/actuator/health").send().await.text();
    assert!(body.contains("teams"), "{body}");
    assert!(body.contains("UP"), "{body}");
    post(&client, &message("hi")).await.assert_status(200);
}

#[test]
fn plugin_name_contract_and_conformance() {
    use autumn_web::plugin::Plugin;
    use autumn_web::plugin_conformance::{ConformanceConfig, run_conformance};
    use autumn_web::route_listing::{RouteClassification, RouteSource};
    let p = TeamsPlugin::new();
    assert_eq!(p.name(), "autumn-plugin-teams");
    assert!(format!("{p:?}").contains("TeamsPlugin"));
    let contract = p.contract().unwrap();
    assert!(format!("{contract:?}").contains("0.8"), "{contract:?}");
    let app = autumn_web::app().plugin(TeamsPlugin::default());
    assert!(app.has_plugin("autumn-plugin-teams"));
    assert!(app.has_config_section("teams"));
    let routes = app.plugin_route_infos().unwrap();
    let r = routes.iter().find(|r| r.path == "/teams/messages").unwrap();
    assert_eq!(r.method, "POST");
    assert_eq!(r.classification, RouteClassification::Public);
    assert_eq!(
        r.source,
        RouteSource::Plugin("autumn-plugin-teams".to_owned())
    );
    let config = ConformanceConfig::new("autumn-plugin-teams")
        .prefix("/teams")
        .contract(contract);
    let report = run_conformance(&config, &routes);
    assert!(report.passed(), "{}", report.to_text_report());
}

#[tokio::test(flavor = "multi_thread")]
async fn custom_base_path_serves_the_route() {
    let t = MemoryTransport::new();
    let client = TestApp::new()
        .plugin(plugin(&t).base_path("/bots/teams/"))
        .build();
    let a = message("hi");
    let auth = authority().bearer(&a, now_secs());
    client
        .post("/bots/teams/messages")
        .header("authorization", &auth)
        .header("content-type", "application/json")
        .body(serde_json::to_string(&a).unwrap())
        .send()
        .await
        .assert_status(200);
}

// ---------------------------------------------------------- startup checks (AC13)

#[tokio::test]
async fn missing_app_id_or_password_fails_start_in_a_web_role() {
    let t = MemoryTransport::new();
    let mut cfg = config();
    "TEAMS_TEST_NO_SUCH_ID".clone_into(&mut cfg.app_id_env);
    let e = TeamsPlugin::with_config(cfg.clone())
        .with_app_password(PASSWORD)
        .with_transport(t.clone())
        .start(&AppState::for_test())
        .unwrap_err();
    assert!(e.to_string().contains("TEAMS_TEST_NO_SUCH_ID"), "{e}");
    let mut cfg = config();
    "TEAMS_TEST_NO_SUCH_PASSWORD".clone_into(&mut cfg.app_password_env);
    let e = TeamsPlugin::with_config(cfg.clone())
        .with_app_id(APP_ID)
        .with_transport(t.clone())
        .start(&AppState::for_test())
        .unwrap_err();
    assert!(e.to_string().contains("TEAMS_TEST_NO_SUCH_PASSWORD"), "{e}");
    // A blank value counts as missing.
    let e = TeamsPlugin::with_config(config())
        .with_app_id("  ")
        .with_app_password(PASSWORD)
        .start(&AppState::for_test())
        .unwrap_err();
    assert!(matches!(e, TeamsError::Config(_)));
    // A worker serves no route: no app ID check, no password check.
    TeamsPlugin::with_config(cfg)
        .with_transport(t)
        .start_with_role(&AppState::for_test(), ProcessRole::Worker)
        .unwrap();
}

#[tokio::test]
async fn invalid_config_or_handlers_fail_start() {
    let mut cfg = config();
    cfg.invoke_timeout_ms = 0;
    let e = TeamsPlugin::with_config(cfg)
        .with_app_id(APP_ID)
        .with_app_password(PASSWORD)
        .start(&AppState::for_test())
        .unwrap_err();
    assert!(e.to_string().contains("invoke_timeout_ms"), "{e}");
    let t = MemoryTransport::new();
    let e = plugin(&t)
        .command("go", |_c, _a| async { Ok(()) })
        .command("Go", |_c, _a| async { Ok(()) })
        .start(&AppState::for_test())
        .unwrap_err();
    assert!(e.to_string().contains("registered twice"), "{e}");
    let e = plugin(&t)
        .command("two words", |_c, _a| async { Ok(()) })
        .start(&AppState::for_test())
        .unwrap_err();
    assert!(matches!(e, TeamsError::Config(_)));
}

#[tokio::test]
async fn csrf_without_exemption_fails_start_with_the_fix() {
    let t = MemoryTransport::new();
    let mut cfg = AutumnConfig::default();
    cfg.security.csrf.enabled = true;
    let e = plugin(&t).start(&state_with(cfg.clone())).unwrap_err();
    let text = e.to_string();
    assert!(text.contains("CSRF"), "{text}");
    assert!(text.contains("exempt_paths"), "{text}");
    assert!(text.contains("\"/teams\""), "{text}");
    // A worker needs no exemption.
    plugin(&t)
        .start_with_role(&state_with(cfg.clone()), ProcessRole::Worker)
        .unwrap();
    cfg.security.csrf.exempt_paths = vec!["/teams".into()];
    plugin(&t).start(&state_with(cfg)).unwrap();
}

#[test]
fn failed_startup_stops_the_app_with_the_reason() {
    let t = MemoryTransport::new();
    let mut c = config();
    "TEAMS_TEST_NO_SUCH_PASSWORD_2".clone_into(&mut c.app_password_env);
    let p = TeamsPlugin::with_config(c)
        .with_app_id(APP_ID)
        .with_transport(t);
    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drop(TestApp::new().plugin(p).build());
    }));
    let msg = built.expect_err("startup must fail");
    let text = msg
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| msg.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_default();
    assert!(text.contains("TEAMS_TEST_NO_SUCH_PASSWORD_2"), "{text}");
}

// ------------------------------------------------------------------- keys (AC4)

#[tokio::test]
async fn keys_are_cached_and_refreshed_after_the_max_age() {
    let t = MemoryTransport::new();
    let keys = CountingKeys::new(keys_of(&authority()));
    let clock = Arc::new(TickingClock::starting_at(now()));
    let rt = plugin(&t)
        .with_key_source(keys.clone())
        .with_clock(clock.clone())
        .start(&AppState::for_test())
        .unwrap();
    let mut a = message("x");
    for i in 0..3 {
        a.id = Some(format!("k{i}"));
        assert_eq!(send(&rt, &a).await, 200);
    }
    assert_eq!(keys.loads.load(Ordering::SeqCst), 1);
    clock.advance(Duration::from_secs(86_400));
    a.id = Some("k-late".into());
    let at = now_secs() + 86_400;
    // A stale set still serves a known kid. One refresh runs in the background.
    assert_eq!(send_as(&rt, &authority(), &a, at).await, 200);
    let m = rt.metrics();
    wait_until(Duration::from_secs(5), &|| m.key_refreshes("ok") == 2).await;
    assert_eq!(keys.loads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn unknown_kid_refreshes_at_most_once_per_interval() {
    let t = MemoryTransport::new();
    let keys = CountingKeys::new(keys_of(&authority()));
    let clock = Arc::new(TickingClock::starting_at(now()));
    let rt = plugin(&t)
        .with_key_source(keys.clone())
        .with_clock(clock.clone())
        .start(&AppState::for_test())
        .unwrap();
    let a = message("x");
    let unknown = TestAuthority::new(APP_ID).with_kid("rotated");
    // An attacker floods unknown kids. The first load also uses up the
    // budget for unknown kids, so no more loads follow.
    for _ in 0..5 {
        assert_eq!(send_as(&rt, &unknown, &a, now_secs()).await, 401);
    }
    assert_eq!(keys.loads.load(Ordering::SeqCst), 1);
    // Microsoft adds the key. After min_refresh_secs (60 s), the next
    // unknown kid refreshes and passes.
    keys.keys.lock().unwrap().clone_from(&KeySet::new(vec![
        authority().signing_key(),
        unknown.signing_key(),
    ]));
    clock.advance(Duration::from_secs(59));
    assert_eq!(send_as(&rt, &unknown, &a, now_secs() + 59).await, 401);
    assert_eq!(keys.loads.load(Ordering::SeqCst), 1);
    clock.advance(Duration::from_secs(1));
    assert_eq!(send_as(&rt, &unknown, &a, now_secs() + 60).await, 200);
    assert_eq!(keys.loads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn failed_refresh_keeps_the_old_keys() {
    let t = MemoryTransport::new();
    let keys = CountingKeys::new(keys_of(&authority()));
    let clock = Arc::new(TickingClock::starting_at(now()));
    let rt = plugin(&t)
        .with_key_source(keys.clone())
        .with_clock(clock.clone())
        .start(&AppState::for_test())
        .unwrap();
    let mut a = message("x");
    a.id = Some("1".into());
    assert_eq!(send(&rt, &a).await, 200);
    keys.fail.store(true, Ordering::SeqCst);
    clock.advance(Duration::from_secs(86_400));
    a.id = Some("2".into());
    assert_eq!(
        send_as(&rt, &authority(), &a, now_secs() + 86_400).await,
        200
    );
    let m = rt.metrics();
    wait_until(Duration::from_secs(5), &|| m.key_refreshes("error") == 1).await;
    // No keys at all: 503, so the Connector retries.
    let empty = CountingKeys::new(KeySet::default());
    empty.fail.store(true, Ordering::SeqCst);
    let rt = plugin(&t)
        .with_key_source(empty)
        .start(&AppState::for_test())
        .unwrap();
    assert_eq!(send(&rt, &a).await, 503);
}

#[tokio::test]
async fn openid_source_reads_metadata_then_jwks() {
    let t = MemoryTransport::new();
    let a = authority();
    t.respond(
        "GET https://login.botframework.com/v1/.well-known/openidconfiguration",
        HttpReply::json(
            200,
            &json!({"issuer": "https://api.botframework.com", "jwks_uri": "https://login.botframework.com/v1/.well-known/keys"}),
        ),
    );
    let mut jwks = a.jwks();
    // Non-RSA and kid-less keys are skipped.
    jwks["keys"]
        .as_array_mut()
        .unwrap()
        .push(json!({"kty": "EC", "kid": "ec", "crv": "P-256", "x": "a", "y": "b"}));
    jwks["keys"]
        .as_array_mut()
        .unwrap()
        .push(json!({"kty": "RSA", "n": "a", "e": "AQAB"}));
    t.respond(
        "GET https://login.botframework.com/v1/.well-known/keys",
        HttpReply::json(200, &jwks),
    );
    let src = OpenIdKeySource::new(Arc::new(t.clone()), testing_metadata_url());
    let set = src.fetch().await.unwrap();
    assert_eq!(set.len(), 1);
    assert_eq!(set.get(testing::KEY_ID).unwrap().endorsements, ["msteams"]);
    // The default plugin uses this source with the transport.
    let rt = TeamsPlugin::with_config(config())
        .with_app_id(APP_ID)
        .with_app_password(PASSWORD)
        .with_transport(t.clone())
        .with_clock(Arc::new(autumn_web::time::FixedClock::at(now())))
        .start(&AppState::for_test())
        .unwrap();
    assert_eq!(send(&rt, &message("x")).await, 200);
    // A plain-http jwks_uri is refused.
    t.respond(
        "GET https://login.botframework.com/v1/.well-known/openidconfiguration",
        HttpReply::json(
            200,
            &json!({"jwks_uri": "http://login.botframework.com/keys"}),
        ),
    );
    assert!(src.fetch().await.is_err());
    t.respond(
        "GET https://login.botframework.com/v1/.well-known/openidconfiguration",
        HttpReply::text(500, ""),
    );
    assert!(src.fetch().await.is_err());
    assert!(KeySet::from_jwks(&json!({"nokeys": 1})).is_err());
    assert!(format!("{src:?}").contains("openidconfiguration"));
    let _ = StaticKeys(KeySet::default());
}

fn testing_metadata_url() -> String {
    config().openid_metadata_url
}

// ------------------------------------------------------------------ health (AC12)

#[tokio::test]
async fn health_unknown_before_start_and_without_password() {
    let t = MemoryTransport::new();
    let mut cfg = config();
    "TEAMS_TEST_NO_SUCH_PASSWORD".clone_into(&mut cfg.app_password_env);
    let rt = TeamsPlugin::with_config(cfg)
        .with_transport(t)
        .start_with_role(&AppState::for_test(), ProcessRole::Worker)
        .unwrap();
    let out = rt.health().check().await;
    assert_eq!(out.status, HealthStatus::Unknown);
    assert_eq!(out.details["reason"], json!("no app password"));
    assert_eq!(rt.health().group(), IndicatorGroup::HealthOnly);
}

#[tokio::test(start_paused = true)]
async fn health_up_down_cached_and_no_secrets() {
    let t = MemoryTransport::new();
    let rt = plugin(&t)
        .readiness(true)
        .start(&AppState::for_test())
        .unwrap();
    let h = rt.health();
    assert_eq!(h.group(), IndicatorGroup::Readiness);
    assert_eq!(h.check().await.status, HealthStatus::Up);
    assert_eq!(h.check().await.status, HealthStatus::Up);
    assert_eq!(t.requests_to("/oauth2/v2.0/token").len(), 1);
    // A bad secret: Down with a short code.
    let t = MemoryTransport::new();
    t.respond(
        "/oauth2/v2.0/token",
        HttpReply::json(
            401,
            &json!({"error": "invalid_client", "error_description": PASSWORD}),
        ),
    );
    let rt = plugin(&t).start(&AppState::for_test()).unwrap();
    let out = rt.health().check().await;
    assert_eq!(out.status, HealthStatus::Down);
    assert_eq!(out.details["error"], json!("invalid_client"));
    assert!(!format!("{out:?}").contains(PASSWORD));
    // Down stays 5 s or less.
    let n = t.requests_to("/oauth2/v2.0/token").len();
    rt.health().check().await;
    assert_eq!(t.requests_to("/oauth2/v2.0/token").len(), n);
    tokio::time::advance(Duration::from_secs(6)).await;
    rt.health().check().await;
    assert_eq!(t.requests_to("/oauth2/v2.0/token").len(), n + 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn health_check_is_single_flight() {
    let t = MemoryTransport::new();
    t.set_latency(Duration::from_millis(50));
    let mut cfg = config();
    cfg.health.cache_secs = 0;
    let rt = plugin_with(&t, cfg).start(&AppState::for_test()).unwrap();
    let health = rt.health();
    let results = tokio::join!(health.check(), health.check(), health.check());
    for out in <[_; 3]>::from(results) {
        assert_eq!(out.status, HealthStatus::Up);
    }
    assert_eq!(t.requests_to("/oauth2/v2.0/token").len(), 1);
}

// ----------------------------------------------------------------- metrics (AC12)

#[tokio::test(flavor = "multi_thread")]
async fn metrics_count_requests_handlers_and_calls() {
    let t = MemoryTransport::new();
    let p = plugin(&t).command("hi", |ctx, _a| async move {
        ctx.reply(Activity::message("hello")).await?;
        Ok(())
    });
    let metrics = p.metrics();
    let client = TestApp::new().plugin(p).build();
    post(&client, &message("hi")).await.assert_status(200);
    post_raw(&client, None, "{}").await.assert_status(401);
    wait_until(Duration::from_secs(5), &|| {
        metrics.api_calls("reply_to_activity", "ok") == 1
    })
    .await;
    assert_eq!(metrics.requests("ok"), 1);
    assert_eq!(metrics.requests("missing_token"), 1);
    assert_eq!(metrics.handler_runs("command", "ok"), 1);
    assert_eq!(metrics.token_requests("ok"), 1);
    assert_eq!(metrics.key_refreshes("ok"), 1);
    let prom = client.get("/actuator/prometheus").send().await.text();
    assert!(prom.contains("teams_requests_total"), "{prom}");
    assert!(prom.contains("teams_api_calls_total"), "{prom}");
}

#[tokio::test(flavor = "multi_thread")]
async fn metric_labels_never_hold_activity_data() {
    let t = MemoryTransport::new();
    let p = plugin(&t)
        .on_message(|_c, _a| async { Ok(()) })
        .invoke("x/y", |_c, _a| async {
            Ok(autumn_plugin_teams::activity::InvokeResponse::ok())
        });
    let metrics = p.metrics();
    let client = TestApp::new().plugin(p).build();
    let mut m = message("SECRET-TEXT");
    m.id = Some("SECRET-ID".into());
    post(&client, &m).await.assert_status(200);
    let mut i = testing::invoke("x/y", json!({}));
    i.id = Some("SECRET-INVOKE".into());
    post(&client, &i).await.assert_status(200);
    let mut odd = testing::inbound("SECRET-TYPE");
    odd.id = Some("o".into());
    post(&client, &odd).await.assert_status(200);
    let all = format!("{:?}", metrics.collect());
    for s in ["SECRET", "x/y", CONVERSATION_ID, TENANT_ID] {
        assert!(!all.contains(s), "{s} in {all}");
    }
}

// ----------------------------------------------------------------- drain (AC11)

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_waits_for_in_flight_handlers_then_replies_503() {
    let t = MemoryTransport::new();
    let gate = Gate::default();
    let done = Arc::new(AtomicBool::new(false));
    let (g, d) = (gate.clone(), Arc::clone(&done));
    let rt = plugin(&t)
        .on_message(move |_c, _a| {
            let (g, d) = (g.clone(), Arc::clone(&d));
            async move {
                g.wait().await;
                d.store(true, Ordering::SeqCst);
                Ok(())
            }
        })
        .start(&AppState::for_test())
        .unwrap();
    assert_eq!(send(&rt, &message("x")).await, 200);
    wait_until(Duration::from_secs(5), &|| rt.in_flight() == 1).await;
    let opener = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        gate.open();
    });
    within(rt.shutdown()).await;
    assert!(done.load(Ordering::SeqCst), "shutdown did not wait");
    opener.await.unwrap();
    let mut m = message("after");
    m.id = Some("after".into());
    assert_eq!(send(&rt, &m).await, 503);
    assert_eq!(rt.metrics().requests("shutting_down"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_gives_up_after_the_drain_timeout() {
    let t = MemoryTransport::new();
    let mut cfg = config();
    cfg.drain_timeout_secs = 1;
    let rt = plugin_with(&t, cfg)
        .on_message(|_c, _a| async {
            futures::future::pending::<()>().await;
            Ok(())
        })
        .start(&AppState::for_test())
        .unwrap();
    assert_eq!(send(&rt, &message("x")).await, 200);
    let start = std::time::Instant::now();
    within(rt.shutdown()).await;
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(format!("{rt:?}").contains("in_flight"));
}
