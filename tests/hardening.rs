//! Regressions from the review round. Each test names the finding it covers.
#![allow(clippy::unwrap_used, clippy::expect_used)] // Test helpers fail loudly.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

use autumn_plugin_teams::activity::{Activity, ChannelAccount, InvokeResponse, types};
use autumn_plugin_teams::auth::{KeySet, KeySource};
use autumn_plugin_teams::testing::{self, TestAuthority};
use autumn_plugin_teams::transport::{HttpReply, MemoryTransport, Method};
use autumn_plugin_teams::{HandlerError, TeamsError, TeamsPlugin, TeamsRuntime};
use autumn_web::AppState;
use autumn_web::test::{TestApp, TestClient};
use autumn_web::time::TickingClock;
use autumn_web::webhook::{WebhookReplayFuture, WebhookReplayStore, WebhookReplayStoreError};
use common::*;
use futures::future::BoxFuture;
use jsonwebtoken::Algorithm;
use serde_json::{Value, json};
use tokio::sync::mpsc;

fn app(p: TeamsPlugin) -> TestClient {
    TestApp::new().plugin(p).build()
}

/// A plugin that sends one value per handler run, for each background kind.
fn counting(t: &MemoryTransport) -> (TeamsPlugin, mpsc::UnboundedReceiver<&'static str>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let send = |label: &'static str| {
        let tx = tx.clone();
        move |_c, _a| {
            let tx = tx.clone();
            async move {
                tx.send(label).unwrap();
                Ok(())
            }
        }
    };
    let p = plugin(t)
        .on_message(send("message"))
        .on_reaction(send("reaction"))
        .on_event("e", send("event"))
        .on_conversation_update(send("update"))
        .on_message_update(send("edited"))
        .on_message_delete(send("deleted"));
    (p, rx)
}

async fn none_left<T>(rx: &mut mpsc::UnboundedReceiver<T>) {
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(rx.try_recv().is_err(), "an extra handler ran");
}

// ------------------------------------------------- security: auth labels

#[tokio::test(flavor = "multi_thread")]
async fn each_failed_check_has_its_own_label() {
    let t = MemoryTransport::new();
    let mut cfg = config();
    cfg.allowed_tenants = vec![TENANT_ID.to_owned()];
    let p = plugin_with(&t, cfg).on_message(|_c, _a| async { Ok(()) });
    let metrics = p.metrics();
    let client = app(p);
    let with = |k: &str, v: Value| {
        let mut out = claims();
        out[k] = v;
        out
    };
    let n = now_secs();
    let cases: Vec<(Value, &str)> = vec![
        (with("iss", json!("https://evil.example")), "bad_issuer"),
        (with("aud", json!("other")), "bad_audience"),
        (with("exp", json!(n - 400)), "expired"),
        (
            with("serviceurl", json!("https://smba.trafficmanager.net/x/")),
            "service_url_mismatch",
        ),
    ];
    for (c, label) in cases {
        post_claims(&client, &message("x"), &c).await;
        assert_eq!(metrics.requests(label), 1, "{label}");
    }
    let rogue = TestAuthority::rogue(APP_ID);
    let a = message("x");
    post_raw(
        &client,
        Some(&rogue.bearer(&a, n)),
        &serde_json::to_string(&a).unwrap(),
    )
    .await;
    assert_eq!(metrics.requests("bad_signature"), 1);
    let other = TestAuthority::new(APP_ID).with_kid("nope");
    post_raw(
        &client,
        Some(&other.bearer(&a, n)),
        &serde_json::to_string(&a).unwrap(),
    )
    .await;
    assert_eq!(metrics.requests("unknown_key"), 1);
    let mut theirs = message("x");
    theirs.channel_data = Some(json!({"tenant": {"id": "t2"}}));
    post(&client, &theirs).await.assert_status(403);
    assert_eq!(metrics.requests("tenant_not_allowed"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn rs384_with_the_right_key_is_still_refused() {
    let t = MemoryTransport::new();
    let p = plugin(&t).on_message(|_c, _a| async { Ok(()) });
    let metrics = p.metrics();
    let client = app(p);
    for alg in [Algorithm::RS384, Algorithm::RS512, Algorithm::PS256] {
        let token = authority().sign_with(alg, &claims());
        let a = message("x");
        post_raw(
            &client,
            Some(&format!("Bearer {token}")),
            &serde_json::to_string(&a).unwrap(),
        )
        .await
        .assert_status(401);
    }
    assert_eq!(metrics.requests("bad_algorithm"), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_bearer_scheme_is_accepted() {
    let t = MemoryTransport::new();
    let p = plugin(&t).on_message(|_c, _a| async { Ok(()) });
    let metrics = p.metrics();
    let client = app(p);
    let a = message("x");
    let token = authority().sign(&claims());
    for scheme in ["Basic", "Token", "JWT"] {
        post_raw(
            &client,
            Some(&format!("{scheme} {token}")),
            &serde_json::to_string(&a).unwrap(),
        )
        .await
        .assert_status(401);
    }
    assert_eq!(metrics.requests("malformed_token"), 3);
    // RFC 7235: the scheme is case-insensitive.
    post_raw(
        &client,
        Some(&format!("bEaReR {token}")),
        &serde_json::to_string(&a).unwrap(),
    )
    .await
    .assert_status(200);
}

#[tokio::test(flavor = "multi_thread")]
async fn token_header_size_limit_is_enforced_alone() {
    let t = MemoryTransport::new();
    let p = plugin(&t).on_message(|_c, _a| async { Ok(()) });
    let metrics = p.metrics();
    let client = app(p);
    let big = |pad: usize| {
        let mut c = claims();
        c["pad"] = json!("x".repeat(pad));
        format!("Bearer {}", authority().sign(&c))
    };
    let body = serde_json::to_string(&message("x")).unwrap();
    post_raw(&client, Some(&big(8_000)), &body)
        .await
        .assert_status(200);
    post_raw(&client, Some(&big(13_000)), &body)
        .await
        .assert_status(401);
    assert_eq!(metrics.requests("malformed_token"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn channel_allowlist_is_checked_after_the_endorsement() {
    let t = MemoryTransport::new();
    let both = TestAuthority::new(APP_ID).with_endorsements(&["msteams", "webchat"]);
    let p = plugin(&t)
        .with_key_source(both.key_source())
        .on_message(|_c, _a| async { Ok(()) });
    let metrics = p.metrics();
    let client = app(p);
    let mut web = message("x");
    web.channel_id = Some("webchat".into());
    let auth = both.bearer(&web, now_secs());
    post_raw(&client, Some(&auth), &serde_json::to_string(&web).unwrap())
        .await
        .assert_status(403);
    assert_eq!(metrics.requests("channel_not_allowed"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn unsigned_large_body_gets_401_not_413() {
    let t = MemoryTransport::new();
    let mut c = config();
    c.max_body_bytes = 64;
    let client = app(plugin_with(&t, c));
    post_raw(&client, None, &"x".repeat(10_000))
        .await
        .assert_status(401);
}

// ----------------------------------------------------- protocol: invokes

#[tokio::test(flavor = "multi_thread")]
async fn card_action_failures_keep_http_200_with_an_error_body() {
    let t = MemoryTransport::new();
    let gate = Gate::default();
    let opener = gate.clone();
    let mut cfg = slow_config();
    "Try later.".clone_into(&mut cfg.error_text);
    let client = app(plugin_with(&t, cfg)
        .card_action("fail", |_c, _a| async {
            Err::<InvokeResponse, HandlerError>("secret detail".into())
        })
        .card_action("boom", |_c, _a| async { panic!("boom detail") })
        .card_action("slow", move |_c, _a| {
            let g = opener.clone();
            async move {
                g.wait().await;
                Ok(InvokeResponse::ok())
            }
        }));
    for (verb, code) in [
        ("fail", 500),
        ("boom", 500),
        ("slow", 500),
        ("unknown", 400),
    ] {
        let res = post(&client, &testing::card_action(verb, json!({}))).await;
        res.assert_status(200);
        let v: Value = res.json();
        assert_eq!(v["statusCode"], code, "{verb}");
        assert_eq!(v["type"], "application/vnd.microsoft.error", "{verb}");
        assert_eq!(v["value"]["message"], "Try later.", "{verb}");
        assert!(!res.text().contains("detail"), "{verb}");
    }
    gate.open();
}

#[tokio::test(flavor = "multi_thread")]
async fn other_invoke_errors_give_500_with_an_empty_body() {
    let t = MemoryTransport::new();
    let client = app(plugin(&t).invoke("x/fail", |_c, _a| async {
        Err::<InvokeResponse, HandlerError>("secret".into())
    }));
    let r = post(&client, &testing::invoke("x/fail", json!({}))).await;
    r.assert_status(500);
    assert!(r.text().is_empty(), "{}", r.text());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_repeat_invoke_runs_again_and_gets_its_reply() {
    let t = MemoryTransport::new();
    let runs = Arc::new(AtomicU32::new(0));
    let r2 = Arc::clone(&runs);
    let client = app(plugin(&t).invoke("task/fetch", move |_c, _a| {
        let r = Arc::clone(&r2);
        async move {
            r.fetch_add(1, Ordering::SeqCst);
            Ok(InvokeResponse::task_message("hi"))
        }
    }));
    let mut i = testing::invoke("task/fetch", json!({}));
    i.id = Some("same".into());
    for _ in 0..2 {
        let r = post(&client, &i).await;
        r.assert_status(200);
        assert_eq!(r.json::<Value>()["task"]["value"], "hi");
    }
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

// ------------------------------------------------------ protocol: routing

#[tokio::test(flavor = "multi_thread")]
async fn message_update_and_delete_have_handlers() {
    let t = MemoryTransport::new();
    let (p, mut rx) = counting(&t);
    let client = app(p);
    post(&client, &testing::inbound("messageUpdate"))
        .await
        .assert_status(200);
    assert_eq!(recv(&mut rx).await, "edited");
    post(&client, &testing::inbound("messageDelete"))
        .await
        .assert_status(200);
    assert_eq!(recv(&mut rx).await, "deleted");
    none_left(&mut rx).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn command_strips_only_the_bot_mention() {
    let t = MemoryTransport::new();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let client = app(plugin(&t).command("assign", move |_c, a: Activity| {
        let tx = tx.clone();
        async move {
            tx.send(a.command().unwrap().1).unwrap();
            Ok(())
        }
    }));
    let mut m = message("<at>Bot</at> assign <at>Ada</at>");
    m.entities = vec![
        json!({"type": "mention", "text": "<at>Bot</at>", "mentioned": {"id": testing::BOT_ID}}),
        json!({"type": "mention", "text": "<at>Ada</at>", "mentioned": {"id": "29:ada"}}),
    ];
    post(&client, &m).await.assert_status(200);
    assert_eq!(recv(&mut rx).await, "<at>Ada</at>");
}

// ------------------------------------------------------ delivery and dedup

#[tokio::test(flavor = "multi_thread")]
async fn every_background_kind_drops_a_repeat() {
    let t = MemoryTransport::new();
    let (p, mut rx) = counting(&t);
    let client = app(p);
    let mut event = testing::inbound(types::EVENT);
    event.name = Some("e".into());
    let mut update = testing::inbound(types::CONVERSATION_UPDATE);
    update.members_added = vec![ChannelAccount::new("29:n", None)];
    for a in [
        testing::inbound(types::MESSAGE_REACTION),
        event,
        update,
        testing::inbound("messageUpdate"),
    ] {
        post(&client, &a).await.assert_status(200);
        post(&client, &a).await.assert_status(200);
        recv(&mut rx).await;
        none_left(&mut rx).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_repeat_after_the_dedup_window_runs_again() {
    let t = MemoryTransport::new();
    let clock = Arc::new(TickingClock::starting_at(now()));
    let mut c = config();
    c.dedup_window_secs = 60;
    let (tx, mut rx) = mpsc::unbounded_channel::<()>();
    let rt = plugin_with(&t, c)
        .with_clock(clock.clone())
        .on_message(move |_c, _a| {
            let tx = tx.clone();
            async move {
                tx.send(()).unwrap();
                Ok(())
            }
        })
        .start(&AppState::for_test())
        .unwrap();
    let m = message("x");
    assert_eq!(send(&rt, &m, now_secs()).await, 200);
    recv(&mut rx).await;
    clock.advance(Duration::from_secs(61));
    assert_eq!(send(&rt, &m, now_secs() + 61).await, 200);
    recv(&mut rx).await;
}

async fn send(rt: &TeamsRuntime, activity: &Activity, at: u64) -> u16 {
    use autumn_web::reexports::axum::body::Body;
    use autumn_web::reexports::http::Request;
    use tower::ServiceExt as _;
    let req = Request::post("/messages")
        .header("authorization", authority().bearer(activity, at))
        .body(Body::from(serde_json::to_string(activity).unwrap()))
        .unwrap();
    rt.router().oneshot(req).await.unwrap().status().as_u16()
}

/// A replay store that fails, or never answers.
#[derive(Debug)]
struct BadStore {
    hang: bool,
}

impl WebhookReplayStore for BadStore {
    fn check_and_insert<'a>(
        &'a self,
        _key: &'a str,
        _now: SystemTime,
        _window: Duration,
    ) -> WebhookReplayFuture<'a> {
        let hang = self.hang;
        Box::pin(async move {
            if hang {
                futures::future::pending::<()>().await;
            }
            Err(WebhookReplayStoreError::new("store down"))
        })
    }

    fn remove<'a>(&'a self, _key: &'a str) -> WebhookReplayFuture<'a> {
        Box::pin(async { Ok(true) })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_or_hung_replay_store_still_runs_the_handler() {
    for hang in [false, true] {
        let t = MemoryTransport::new();
        let (tx, mut rx) = mpsc::unbounded_channel::<()>();
        let client = app(plugin(&t)
            .with_replay_store(Arc::new(BadStore { hang }))
            .on_message(move |_c, _a| {
                let tx = tx.clone();
                async move {
                    tx.send(()).unwrap();
                    Ok(())
                }
            }));
        within(post(&client, &message("x")))
            .await
            .assert_status(200);
        recv(&mut rx).await;
    }
}

// ------------------------------------------------------------ app token

#[tokio::test]
async fn a_short_token_keeps_half_its_life() {
    let t = MemoryTransport::new();
    t.respond(
        "/oauth2/v2.0/token",
        HttpReply::json(
            200,
            &json!({"token_type": "Bearer", "expires_in": 400, "access_token": "a"}),
        ),
    );
    let clock = Arc::new(TickingClock::starting_at(now()));
    let c = plugin(&t)
        .with_clock(clock.clone())
        .start(&AppState::for_test())
        .unwrap()
        .client();
    c.check_credentials().await.unwrap();
    clock.advance(Duration::from_secs(150));
    c.check_credentials().await.unwrap();
    assert_eq!(t.requests_to("/oauth2/v2.0/token").len(), 1);
    clock.advance(Duration::from_secs(50));
    c.check_credentials().await.unwrap();
    assert_eq!(t.requests_to("/oauth2/v2.0/token").len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn waiters_share_one_failed_token_request() {
    let t = MemoryTransport::new();
    t.respond("/oauth2/v2.0/token", HttpReply::text(503, ""));
    t.set_latency(Duration::from_millis(20));
    let c = plugin(&t).start(&AppState::for_test()).unwrap().client();
    let tasks: Vec<_> = (0..5)
        .map(|_| {
            let c = c.clone();
            tokio::spawn(async move { c.check_credentials().await })
        })
        .collect();
    for task in tasks {
        let e = task.await.unwrap().unwrap_err();
        assert!(
            matches!(&e, TeamsError::Token { code } if code == "http_503"),
            "{e:?}"
        );
    }
    // One shared sequence of 3 attempts, not 5 sequences.
    assert_eq!(t.requests_to("/oauth2/v2.0/token").len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_old_token_serves_while_a_refresh_runs() {
    let t = MemoryTransport::new();
    t.respond_seq(
        "/oauth2/v2.0/token",
        vec![
            HttpReply::json(
                200,
                &json!({"token_type": "Bearer", "expires_in": 3600, "access_token": "old"}),
            ),
            HttpReply::json(
                200,
                &json!({"token_type": "Bearer", "expires_in": 3600, "access_token": "new"}),
            ),
        ],
    );
    let clock = Arc::new(TickingClock::starting_at(now()));
    let c = plugin(&t)
        .with_clock(clock.clone())
        .start(&AppState::for_test())
        .unwrap()
        .client();
    c.check_credentials().await.unwrap();
    // In the margin, but not expired.
    clock.advance(Duration::from_secs(3_400));
    t.set_latency(Duration::from_millis(500));
    let slow = {
        let c = c.clone();
        tokio::spawn(async move { c.check_credentials().await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let start = std::time::Instant::now();
    t.set_latency(Duration::ZERO);
    c.send_to_conversation(SERVICE_URL, "c", &Activity::message("x"))
        .await
        .unwrap();
    assert!(
        start.elapsed() < Duration::from_millis(300),
        "{:?}",
        start.elapsed()
    );
    let sent = connector_requests(&t);
    assert_eq!(sent[0].header("authorization").unwrap(), "Bearer old");
    slow.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_health_timeout_does_not_cancel_the_token_request() {
    use autumn_web::actuator::{HealthIndicator, HealthStatus};
    let t = MemoryTransport::new();
    t.set_latency(Duration::from_millis(2_000));
    let rt = plugin(&t).start(&AppState::for_test()).unwrap();
    let out = rt.health().check().await;
    assert_eq!(out.status, HealthStatus::Down);
    assert_eq!(out.details["error"], json!("timeout"));
    // The first request goes on in the background and ends at about 2 s.
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    let start = std::time::Instant::now();
    rt.client().check_credentials().await.unwrap();
    assert!(
        start.elapsed() < Duration::from_millis(1_000),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(t.requests_to("/oauth2/v2.0/token").len(), 1);
}

// ----------------------------------------------------------------- keys

/// A key source that counts loads, can fail, and can be slow.
#[derive(Clone)]
struct SlowKeys {
    keys: KeySet,
    loads: Arc<AtomicU32>,
    fail: Arc<std::sync::atomic::AtomicBool>,
    latency_ms: Arc<std::sync::atomic::AtomicU64>,
}

impl SlowKeys {
    fn new(latency_ms: u64) -> Self {
        Self {
            keys: KeySet::new(vec![authority().signing_key()]),
            loads: Arc::default(),
            fail: Arc::default(),
            latency_ms: Arc::new(latency_ms.into()),
        }
    }
}

impl KeySource for SlowKeys {
    fn fetch(&self) -> BoxFuture<'_, Result<KeySet, TeamsError>> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let ms = self.latency_ms.load(Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(ms)).await;
            if self.fail.load(Ordering::SeqCst) {
                Err(TeamsError::Transport("down".into()))
            } else {
                Ok(self.keys.clone())
            }
        })
    }
}

fn keyed(keys: &SlowKeys, clock: &Arc<TickingClock>) -> TeamsRuntime {
    plugin(&MemoryTransport::new())
        .with_key_source(keys.clone())
        .with_clock(clock.clone())
        .on_message(|_c, _a| async { Ok(()) })
        .start(&AppState::for_test())
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cold_key_cache_loads_once_for_many_requests() {
    let keys = SlowKeys::new(100);
    let clock = Arc::new(TickingClock::starting_at(now()));
    let rt = Arc::new(keyed(&keys, &clock));
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let rt = Arc::clone(&rt);
            tokio::spawn(async move { send(&rt, &message("x"), now_secs()).await })
        })
        .collect();
    for task in tasks {
        assert_eq!(task.await.unwrap(), 200);
    }
    assert_eq!(keys.loads.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_keys_serve_while_a_refresh_runs_and_failures_are_rate_limited() {
    let keys = SlowKeys::new(0);
    let clock = Arc::new(TickingClock::starting_at(now()));
    let rt = keyed(&keys, &clock);
    assert_eq!(send(&rt, &message("x"), now_secs()).await, 200);
    keys.fail.store(true, Ordering::SeqCst);
    let day = 86_400;
    clock.advance(Duration::from_secs(day));
    for _ in 0..5 {
        assert_eq!(send(&rt, &message("x"), now_secs() + day).await, 200);
    }
    // The first load, then one failed refresh: the next waits min_refresh_secs.
    rt_wait_loads(&keys, 2).await;
    assert_eq!(keys.loads.load(Ordering::SeqCst), 2);
}

async fn rt_wait_loads(keys: &SlowKeys, n: u32) {
    wait_until(Duration::from_secs(5), &|| {
        keys.loads.load(Ordering::SeqCst) >= n
    })
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_refresh_does_not_block_requests_with_known_keys() {
    let keys = SlowKeys::new(0);
    let clock = Arc::new(TickingClock::starting_at(now()));
    let rt = keyed(&keys, &clock);
    assert_eq!(send(&rt, &message("x"), now_secs()).await, 200);
    keys.latency_ms.store(3_000, Ordering::SeqCst);
    let day = 86_400;
    clock.advance(Duration::from_secs(day));
    let start = std::time::Instant::now();
    assert_eq!(send(&rt, &message("x"), now_secs() + day).await, 200);
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_keys_stop_after_five_days() {
    let keys = SlowKeys::new(0);
    let clock = Arc::new(TickingClock::starting_at(now()));
    let rt = keyed(&keys, &clock);
    assert_eq!(send(&rt, &message("x"), now_secs()).await, 200);
    keys.fail.store(true, Ordering::SeqCst);
    let five_days = 432_000;
    clock.advance(Duration::from_secs(five_days));
    assert_eq!(send(&rt, &message("x"), now_secs() + five_days).await, 503);
}

// ---------------------------------------------------------------- client

#[tokio::test(start_paused = true)]
async fn status_412_is_retried_for_a_send() {
    let t = MemoryTransport::new();
    t.respond_seq(
        "/activities",
        vec![
            HttpReply::text(412, ""),
            HttpReply::json(201, &json!({"id": "ok"})),
        ],
    );
    let c = plugin(&t).start(&AppState::for_test()).unwrap().client();
    let r = c
        .send_to_conversation(SERVICE_URL, "c", &Activity::message("x"))
        .await
        .unwrap();
    assert_eq!(r.id, "ok");
    assert_eq!(connector_requests(&t).len(), 2);
}

#[tokio::test]
async fn page_size_is_clamped_to_the_connector_range() {
    let t = MemoryTransport::new();
    t.respond(
        "/pagedmembers",
        HttpReply::json(200, &json!({"members": []})),
    );
    let c = plugin(&t).start(&AppState::for_test()).unwrap().client();
    c.get_paged_members(SERVICE_URL, "c", Some(10), None)
        .await
        .unwrap();
    c.get_paged_members(SERVICE_URL, "c", Some(1_000), None)
        .await
        .unwrap();
    let urls: Vec<_> = connector_requests(&t).into_iter().map(|r| r.url).collect();
    assert!(urls[0].ends_with("pageSize=50"), "{}", urls[0]);
    assert!(urls[1].ends_with("pageSize=500"), "{}", urls[1]);
}

#[tokio::test]
async fn send_to_a_reference_checks_the_service_url() {
    let t = MemoryTransport::new();
    let c = plugin(&t).start(&AppState::for_test()).unwrap().client();
    let mut reference = message("x").conversation_reference().unwrap();
    "https://evil.example/".clone_into(&mut reference.service_url);
    let e = c
        .send_to(&reference, &Activity::message("x"))
        .await
        .unwrap_err();
    assert!(matches!(e, TeamsError::BadServiceUrl), "{e:?}");
    assert!(t.requests().is_empty());
}

#[tokio::test]
async fn the_request_url_is_the_parsed_url() {
    let t = MemoryTransport::new();
    let c = plugin(&t).start(&AppState::for_test()).unwrap().client();
    c.call(
        "x",
        Method::Get,
        "https://SMBA.trafficmanager.net/teams",
        &["v3"],
        &[],
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        connector_requests(&t)[0].url,
        "https://smba.trafficmanager.net/teams/v3"
    );
}
