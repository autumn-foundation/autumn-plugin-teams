//! Inbound activities through the full autumn middleware stack.
#![allow(clippy::unwrap_used, clippy::expect_used)] // Test helpers fail loudly.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use autumn_plugin_teams::activity::{
    Activity, ChannelAccount, InvokeResponse, MessageReaction, types,
};
use autumn_plugin_teams::testing::{self, TestAuthority};
use autumn_plugin_teams::transport::{MEMORY_ACCESS_TOKEN, MemoryTransport, Method};
use autumn_plugin_teams::{HandlerError, TeamsContext, TeamsPlugin};
use autumn_web::test::{TestApp, TestClient};
use common::*;
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// A change to token claims.
type Edit = Box<dyn Fn(&mut Value)>;

fn app(p: TeamsPlugin) -> TestClient {
    TestApp::new().plugin(p).build()
}

/// A plugin whose `on_message` handler sends each text to the channel.
fn echo(t: &MemoryTransport) -> (TeamsPlugin, mpsc::UnboundedReceiver<String>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let p = plugin(t).on_message(move |_ctx, a: Activity| {
        let tx = tx.clone();
        async move {
            tx.send(a.text.unwrap_or_default()).unwrap();
            Ok(())
        }
    });
    (p, rx)
}

/// Fails when the handler ran.
async fn assert_no_handler(rx: &mut mpsc::UnboundedReceiver<String>) {
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(rx.try_recv().is_err(), "a handler ran");
}

// ------------------------------------------------------------- token check (AC2)

#[tokio::test(flavor = "multi_thread")]
async fn missing_or_malformed_token_gives_401_and_no_handler_runs() {
    let t = MemoryTransport::new();
    let (p, mut rx) = echo(&t);
    let client = app(p);
    let body = serde_json::to_string(&message("hi")).unwrap();
    for auth in [
        None,
        Some(""),
        Some("Basic abc"),
        Some("Bearer"),
        Some("Bearer not-a-jwt"),
        Some("bearer a.b.c"),
    ] {
        post_raw(&client, auth, &body).await.assert_status(401);
    }
    // A huge header is not parsed.
    let big = format!("Bearer {}", "a".repeat(20_000));
    post_raw(&client, Some(&big), &body)
        .await
        .assert_status(401);
    assert_no_handler(&mut rx).await;
    assert!(t.requests().is_empty(), "no call before the check");
}

#[tokio::test(flavor = "multi_thread")]
async fn forged_signature_and_unknown_kid_give_401() {
    let t = MemoryTransport::new();
    let (p, mut rx) = echo(&t);
    let client = app(p);
    let a = message("hi");
    let body = serde_json::to_string(&a).unwrap();
    // Same kid, other key.
    let rogue = TestAuthority::rogue(APP_ID).bearer(&a, now_secs());
    post_raw(&client, Some(&rogue), &body)
        .await
        .assert_status(401);
    // Unknown kid.
    let other = TestAuthority::new(APP_ID)
        .with_kid("other")
        .bearer(&a, now_secs());
    post_raw(&client, Some(&other), &body)
        .await
        .assert_status(401);
    // Body changed after signing does not matter (the JWT covers claims),
    // but a token for another app does.
    let other_app = TestAuthority::new("someone-else").bearer(&a, now_secs());
    post_raw(&client, Some(&other_app), &body)
        .await
        .assert_status(401);
    assert_no_handler(&mut rx).await;
}

/// `base64url(json)`.
fn b64(v: &Value) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let bytes = v.to_string().into_bytes();
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n =
            chunk.iter().fold(0u32, |acc, b| (acc << 8) | u32::from(*b)) << (8 * (3 - chunk.len()));
        for i in 0..=chunk.len() {
            out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn none_and_hmac_algorithms_give_401() {
    let t = MemoryTransport::new();
    let (p, mut rx) = echo(&t);
    let client = app(p);
    let body = serde_json::to_string(&message("hi")).unwrap();
    let claims = b64(&claims());
    for alg in ["none", "HS256", "RS384", "PS256"] {
        let header = b64(&json!({"alg": alg, "typ": "JWT", "kid": testing::KEY_ID}));
        let token = format!("Bearer {header}.{claims}.c2ln");
        post_raw(&client, Some(&token), &body)
            .await
            .assert_status(401);
    }
    assert_no_handler(&mut rx).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_issuer_audience_or_missing_claims_give_401() {
    let t = MemoryTransport::new();
    let (p, mut rx) = echo(&t);
    let client = app(p);
    let a = message("hi");
    let edits: Vec<Edit> = vec![
        Box::new(|c| c["iss"] = json!("https://sts.windows.net/evil/")),
        Box::new(|c| c["aud"] = json!("another-app")),
        Box::new(|c| c["aud"] = json!(["another-app"])),
        Box::new(|c| {
            c.as_object_mut().unwrap().remove("exp");
        }),
        Box::new(|c| {
            c.as_object_mut().unwrap().remove("aud");
        }),
        Box::new(|c| {
            c.as_object_mut().unwrap().remove("iss");
        }),
    ];
    for edit in edits {
        let mut c = claims();
        edit(&mut c);
        post_claims(&client, &a, &c).await.assert_status(401);
    }
    assert_no_handler(&mut rx).await;
    // Control: valid claims pass.
    post_claims(&client, &a, &claims()).await.assert_status(200);
    assert_eq!(recv(&mut rx).await, "hi");
}

#[tokio::test(flavor = "multi_thread")]
async fn time_window_uses_skew_edges() {
    let t = MemoryTransport::new();
    let (p, mut rx) = echo(&t);
    let client = app(p);
    let n = now_secs();
    let with = |nbf: u64, exp: u64| {
        let mut c = claims();
        c["nbf"] = json!(nbf);
        c["exp"] = json!(exp);
        c
    };
    // Expired 300 s ago: still in the 5 min skew.
    post_claims(&client, &message("hi"), &with(n - 1_000, n - 300))
        .await
        .assert_status(200);
    assert_eq!(recv(&mut rx).await, "hi");
    // Expired 301 s ago.
    post_claims(&client, &message("hi"), &with(n - 1_000, n - 301))
        .await
        .assert_status(401);
    // Valid in 300 s: in the skew.
    post_claims(&client, &message("hi"), &with(n + 300, n + 3_600))
        .await
        .assert_status(200);
    assert_eq!(recv(&mut rx).await, "hi");
    // Valid in 301 s.
    post_claims(&client, &message("hi"), &with(n + 301, n + 3_600))
        .await
        .assert_status(401);
    assert_no_handler(&mut rx).await;
}

// ------------------------------------------------------- activity checks (AC3)

#[tokio::test(flavor = "multi_thread")]
async fn missing_endorsement_gives_403() {
    let t = MemoryTransport::new();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let skype_only = TestAuthority::new(APP_ID).with_endorsements(&["skype"]);
    let p = plugin(&t)
        .with_key_source(skype_only.key_source())
        .on_message(move |_c, a: Activity| {
            let tx = tx.clone();
            async move {
                tx.send(a.text.unwrap_or_default()).unwrap();
                Ok(())
            }
        });
    let client = app(p);
    post(&client, &message("hi")).await.assert_status(403);
    assert_no_handler(&mut rx).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn service_url_claim_must_match_the_activity() {
    let t = MemoryTransport::new();
    let (p, mut rx) = echo(&t);
    let client = app(p);
    let a = message("hi");
    let mut c = claims();
    c["serviceurl"] = json!("https://smba.trafficmanager.net/emea/");
    post_claims(&client, &a, &c).await.assert_status(403);
    let mut c = claims();
    c.as_object_mut().unwrap().remove("serviceurl");
    post_claims(&client, &a, &c).await.assert_status(403);
    // An activity with an attacker serviceUrl and a token for the real one.
    let mut evil = message("hi");
    evil.service_url = Some("https://evil.example/".to_owned());
    post_claims(&client, &evil, &claims())
        .await
        .assert_status(403);
    assert_no_handler(&mut rx).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tenant_and_channel_allowlists_give_403() {
    let t = MemoryTransport::new();
    let mut c = config();
    c.allowed_tenants = vec![TENANT_ID.to_owned()];
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let p = plugin_with(&t, c).on_message(move |_c, a: Activity| {
        let tx = tx.clone();
        async move {
            tx.send(a.text.unwrap_or_default()).unwrap();
            Ok(())
        }
    });
    let client = app(p);
    post(&client, &message("ours")).await.assert_status(200);
    assert_eq!(recv(&mut rx).await, "ours");
    let mut other = message("theirs");
    other.channel_data = Some(json!({"tenant": {"id": "other-tenant"}}));
    other.conversation.as_mut().unwrap().tenant_id = Some("other-tenant".to_owned());
    post(&client, &other).await.assert_status(403);
    let mut none = message("none");
    none.channel_data = None;
    none.conversation.as_mut().unwrap().tenant_id = None;
    post(&client, &none).await.assert_status(403);
    let mut skype = message("skype");
    skype.channel_id = Some("skype".to_owned());
    post(&client, &skype).await.assert_status(403);
    assert_no_handler(&mut rx).await;
}

// ------------------------------------------------------------ activities (AC5)

#[tokio::test(flavor = "multi_thread")]
async fn ack_does_not_wait_for_a_slow_message_handler() {
    let t = MemoryTransport::new();
    let gate = Gate::default();
    let (tx, mut rx) = mpsc::unbounded_channel::<()>();
    let g = gate.clone();
    let client = app(plugin(&t).on_message(move |_c, _a| {
        let (g, tx) = (g.clone(), tx.clone());
        async move {
            g.wait().await;
            tx.send(()).unwrap();
            Ok(())
        }
    }));
    let r = within(post(&client, &message("slow"))).await;
    r.assert_status(200);
    assert!(rx.try_recv().is_err(), "the ack came after the handler");
    gate.open();
    recv(&mut rx).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn command_routes_by_first_word_ignoring_mention_slash_and_case() {
    let t = MemoryTransport::new();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let (tx2, mut rx2) = mpsc::unbounded_channel::<String>();
    let client = app(plugin(&t)
        .command("deploy", move |_c, a: Activity| {
            let tx = tx.clone();
            async move {
                let (word, rest) = a.command().unwrap();
                tx.send(format!("{word}|{rest}")).unwrap();
                Ok(())
            }
        })
        .on_message(move |_c, a: Activity| {
            let tx2 = tx2.clone();
            async move {
                tx2.send(a.text_without_mentions()).unwrap();
                Ok(())
            }
        }));
    post(&client, &message("<at>Bot</at> Deploy prod now"))
        .await
        .assert_status(200);
    assert_eq!(recv(&mut rx).await, "deploy|prod now");
    post(&client, &message("/DEPLOY")).await.assert_status(200);
    assert_eq!(recv(&mut rx).await, "deploy|");
    post(&client, &message("<at>Bot</at> hello there"))
        .await
        .assert_status(200);
    assert_eq!(recv(&mut rx2).await, "hello there");
    post(&client, &message("deployment"))
        .await
        .assert_status(200);
    assert_eq!(recv(&mut rx2).await, "deployment");
}

#[tokio::test(flavor = "multi_thread")]
async fn handler_reply_goes_to_the_reply_endpoint_with_the_app_token() {
    let t = MemoryTransport::new();
    let client = app(
        plugin(&t).on_message(|ctx: TeamsContext, a: Activity| async move {
            ctx.reply(Activity::message(format!(
                "you said {}",
                a.text_without_mentions()
            )))
            .await?;
            Ok(())
        }),
    );
    let mut m = message("ping");
    m.id = Some("act-1".to_owned());
    post(&client, &m).await.assert_status(200);
    wait_until(Duration::from_secs(5), &|| {
        !connector_requests(&t).is_empty()
    })
    .await;
    let reqs = connector_requests(&t);
    let r = &reqs[0];
    assert_eq!(
        r.url,
        format!(
            "{SERVICE_URL}v3/conversations/19%3Atest-conversation%40thread.tacv2/activities/act-1"
        )
    );
    assert_eq!(
        r.header("authorization").unwrap(),
        format!("Bearer {MEMORY_ACCESS_TOKEN}")
    );
    let body = r.json();
    assert_eq!(body["type"], "message");
    assert_eq!(body["text"], "you said ping");
    assert_eq!(body["replyToId"], "act-1");
    assert_eq!(body["conversation"]["id"], CONVERSATION_ID);
    assert_eq!(body["from"]["id"], testing::BOT_ID);
    assert_eq!(body["recipient"]["id"], testing::USER_ID);
}

#[tokio::test(flavor = "multi_thread")]
async fn message_handler_error_or_panic_replies_error_text() {
    let t = MemoryTransport::new();
    let client = app(plugin(&t)
        .command("fail", |_c, _a| async {
            Err::<(), HandlerError>("db password=hunter2 is wrong".into())
        })
        .command("boom", |_c, _a| async { panic!("boom") }));
    for word in ["fail", "boom"] {
        let before = connector_requests(&t).len();
        post(&client, &message(word)).await.assert_status(200);
        wait_until(Duration::from_secs(5), &|| {
            connector_requests(&t).len() > before
        })
        .await;
        let body = connector_requests(&t).last().unwrap().json();
        assert_eq!(body["text"], config().error_text);
        assert!(!body.to_string().contains("hunter2"));
    }
    // The app still serves.
    post(&client, &message("fail")).await.assert_status(200);
}

#[tokio::test(flavor = "multi_thread")]
async fn conversation_update_runs_member_and_update_handlers() {
    let t = MemoryTransport::new();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let (t1, t2, t3) = (tx.clone(), tx.clone(), tx);
    let client = app(plugin(&t)
        .on_members_added(move |_c, a: Activity| {
            let tx = t1.clone();
            async move {
                let ids: Vec<_> = a
                    .members_added_except_bot()
                    .iter()
                    .map(|m| m.id.clone())
                    .collect();
                tx.send(format!("added {}", ids.join(","))).unwrap();
                Ok(())
            }
        })
        .on_members_removed(move |_c, a: Activity| {
            let tx = t2.clone();
            async move {
                tx.send(format!("removed {}", a.members_removed.len()))
                    .unwrap();
                Ok(())
            }
        })
        .on_conversation_update(move |_c, a: Activity| {
            let tx = t3.clone();
            async move {
                tx.send(format!("update {}", a.event_type().unwrap_or("-")))
                    .unwrap();
                Ok(())
            }
        }));
    let mut up = testing::inbound(types::CONVERSATION_UPDATE);
    up.members_added = vec![
        ChannelAccount::new(testing::BOT_ID, None),
        ChannelAccount::new("29:new", None),
    ];
    up.channel_data = Some(json!({"tenant": {"id": TENANT_ID}, "eventType": "teamMemberAdded"}));
    post(&client, &up).await.assert_status(200);
    let mut got = vec![recv(&mut rx).await, recv(&mut rx).await];
    got.sort();
    assert_eq!(got, ["added 29:new", "update teamMemberAdded"]);
    let mut down = testing::inbound(types::CONVERSATION_UPDATE);
    down.id = Some("2".to_owned());
    down.members_removed = vec![ChannelAccount::new("29:old", None)];
    post(&client, &down).await.assert_status(200);
    let mut got = vec![recv(&mut rx).await, recv(&mut rx).await];
    got.sort();
    assert_eq!(got, ["removed 1", "update -"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn reaction_installation_and_event_handlers_run() {
    let t = MemoryTransport::new();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let (t1, t2, t3) = (tx.clone(), tx.clone(), tx);
    let client = app(plugin(&t)
        .on_reaction(move |_c, a: Activity| {
            let tx = t1.clone();
            async move {
                tx.send(format!("reaction {}", a.reactions_added[0].kind))
                    .unwrap();
                Ok(())
            }
        })
        .on_installation_update(move |_c, a: Activity| {
            let tx = t2.clone();
            async move {
                tx.send(format!("install {}", a.action.unwrap_or_default()))
                    .unwrap();
                Ok(())
            }
        })
        .on_event("reminder", move |_c, a: Activity| {
            let tx = t3.clone();
            async move {
                tx.send(format!("event {}", a.value.unwrap_or_default()))
                    .unwrap();
                Ok(())
            }
        }));
    let mut r = testing::inbound(types::MESSAGE_REACTION);
    r.id = Some("r1".into());
    let mut like = MessageReaction::default();
    "like".clone_into(&mut like.kind);
    r.reactions_added = vec![like];
    post(&client, &r).await.assert_status(200);
    assert_eq!(recv(&mut rx).await, "reaction like");
    let mut i = testing::inbound(types::INSTALLATION_UPDATE);
    i.id = Some("i1".into());
    i.action = Some("add".into());
    post(&client, &i).await.assert_status(200);
    assert_eq!(recv(&mut rx).await, "install add");
    let mut e = testing::inbound(types::EVENT);
    e.id = Some("e1".into());
    e.name = Some("reminder".into());
    e.value = Some(json!(7));
    post(&client, &e).await.assert_status(200);
    assert_eq!(recv(&mut rx).await, "event 7");
    // An event with another name has no handler.
    e.id = Some("e2".into());
    e.name = Some("other".into());
    post(&client, &e).await.assert_status(200);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(rx.try_recv().is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn repeat_activity_id_runs_the_handler_once() {
    let t = MemoryTransport::new();
    let (p, mut rx) = echo(&t);
    let metrics = p.metrics();
    let client = app(p);
    let mut m = message("once");
    m.id = Some("dup-1".into());
    post(&client, &m).await.assert_status(200);
    post(&client, &m).await.assert_status(200);
    assert_eq!(recv(&mut rx).await, "once");
    assert_no_handler(&mut rx).await;
    assert_eq!(metrics.requests("duplicate"), 1);
    // The same ID in another conversation is not a repeat.
    m.conversation.as_mut().unwrap().id = "19:other@thread.tacv2".into();
    post(&client, &m).await.assert_status(200);
    assert_eq!(recv(&mut rx).await, "once");
    // An activity with no ID is never a repeat.
    m.id = None;
    post(&client, &m).await.assert_status(200);
    post(&client, &m).await.assert_status(200);
    assert_eq!(recv(&mut rx).await, "once");
    assert_eq!(recv(&mut rx).await, "once");
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_types_get_200_and_bad_json_gets_400() {
    let t = MemoryTransport::new();
    let (p, mut rx) = echo(&t);
    let metrics = p.metrics();
    let client = app(p);
    let mut typing = testing::inbound(types::TYPING);
    typing.id = Some("t1".into());
    post(&client, &typing).await.assert_status(200);
    let mut odd = testing::inbound("somethingNew");
    odd.id = Some("t2".into());
    post(&client, &odd).await.assert_status(200);
    assert!(metrics.requests("unhandled") >= 2);
    // Bad JSON with a valid token.
    let auth = authority().bearer(&message("x"), now_secs());
    post_raw(&client, Some(&auth), "{not json")
        .await
        .assert_status(400);
    post_raw(&client, Some(&auth), "{\"text\": \"no type\"}")
        .await
        .assert_status(400);
    assert_no_handler(&mut rx).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn large_body_gives_413() {
    let t = MemoryTransport::new();
    let mut c = config();
    c.max_body_bytes = 64;
    let client = app(plugin_with(&t, c));
    post(&client, &message(&"x".repeat(200)))
        .await
        .assert_status(413);
}

#[tokio::test(flavor = "multi_thread")]
async fn handler_panic_does_not_break_the_next_request() {
    let t = MemoryTransport::new();
    let n = Arc::new(AtomicU32::new(0));
    let n2 = Arc::clone(&n);
    let client = app(plugin(&t).on_message(move |_c, a: Activity| {
        let n = Arc::clone(&n2);
        async move {
            n.fetch_add(1, Ordering::SeqCst);
            assert_ne!(a.text.as_deref(), Some("panic"), "test panic");
            Ok(())
        }
    }));
    let mut m = message("panic");
    m.id = Some("p1".into());
    post(&client, &m).await.assert_status(200);
    m.id = Some("p2".into());
    m.text = Some("fine".into());
    post(&client, &m).await.assert_status(200);
    wait_until(Duration::from_secs(5), &|| n.load(Ordering::SeqCst) == 2).await;
}

// --------------------------------------------------------------- invokes (AC6)

#[tokio::test(flavor = "multi_thread")]
async fn card_action_replies_by_verb() {
    let t = MemoryTransport::new();
    let client = app(
        plugin(&t).card_action("approve", |_c, a: Activity| async move {
            let action = a.card_action().unwrap();
            Ok(InvokeResponse::card_message(format!(
                "approved {}",
                action.data["id"]
            )))
        }),
    );
    let r = post(&client, &testing::card_action("approve", json!({"id": 7}))).await;
    r.assert_status(200);
    let v: Value = r.json();
    assert_eq!(v["statusCode"], 200);
    assert_eq!(v["type"], "application/vnd.microsoft.activity.message");
    assert_eq!(v["value"], "approved 7");
    // Another verb has no handler.
    let mut other = testing::card_action("reject", json!({}));
    other.id = Some("2".into());
    post(&client, &other).await.assert_status(501);
}

#[tokio::test(flavor = "multi_thread")]
async fn invoke_by_name_returns_status_and_body() {
    let t = MemoryTransport::new();
    let client = app(plugin(&t)
        .invoke("task/fetch", |_c, _a| async {
            Ok(InvokeResponse::task_card(
                "Form",
                json!({"type": "AdaptiveCard"}),
            ))
        })
        .invoke("composeExtension/query", |_c, _a| async {
            Ok(InvokeResponse::json(202, json!({"x": 1})))
        })
        .invoke("signin/verifyState", |_c, _a| async {
            Ok(InvokeResponse::ok())
        }));
    let fetch = post(&client, &testing::invoke("task/fetch", json!({}))).await;
    fetch.assert_status(200);
    let v: Value = fetch.json();
    assert_eq!(v["task"]["type"], "continue");
    assert_eq!(v["task"]["value"]["title"], "Form");
    let mut query = testing::invoke("composeExtension/query", json!({}));
    query.id = Some("q".into());
    let queried = post(&client, &query).await;
    queried.assert_status(202);
    assert_eq!(queried.json::<Value>()["x"], 1);
    let mut signin = testing::invoke("signin/verifyState", json!({}));
    signin.id = Some("s".into());
    let signed = post(&client, &signin).await;
    signed.assert_status(200);
    assert!(signed.text().is_empty());
    let mut unknown = testing::invoke("unknown/thing", json!({}));
    unknown.id = Some("u".into());
    post(&client, &unknown).await.assert_status(501);
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_invoke_gets_empty_200_and_counts_a_late_ack() {
    let t = MemoryTransport::new();
    let gate = Gate::default();
    let g = gate.clone();
    let p = plugin_with(&t, slow_config()).invoke("task/fetch", move |_c, _a| {
        let g = g.clone();
        async move {
            g.wait().await;
            Ok(InvokeResponse::task_message("late"))
        }
    });
    let metrics = p.metrics();
    let client = app(p);
    let r = within(post(&client, &testing::invoke("task/fetch", json!({})))).await;
    r.assert_status(200);
    assert!(r.text().is_empty(), "{}", r.text());
    assert_eq!(metrics.late_acks(), 1);
    gate.open();
    wait_until(Duration::from_secs(5), &|| {
        metrics.handler_runs("invoke", "ok") == 1
    })
    .await;
    // A late invoke reply is not sent anywhere.
    assert!(connector_requests(&t).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn invoke_error_or_panic_gives_500_with_no_detail() {
    let t = MemoryTransport::new();
    let client = app(plugin(&t)
        .invoke("a/fail", |_c, _a| async {
            Err::<InvokeResponse, HandlerError>("secret detail".into())
        })
        .card_action("boom", |_c, _a| async { panic!("boom detail") }));
    let r = post(&client, &testing::invoke("a/fail", json!({}))).await;
    r.assert_status(500);
    assert!(!r.text().contains("secret"), "{}", r.text());
    let mut b = testing::card_action("boom", json!({}));
    b.id = Some("b".into());
    let r = post(&client, &b).await;
    r.assert_status(500);
    assert!(!r.text().contains("boom"), "{}", r.text());
}

#[tokio::test(flavor = "multi_thread")]
async fn context_sends_types_updates_and_deletes_in_the_conversation() {
    let t = MemoryTransport::new();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let client = app(plugin(&t).on_message(move |ctx: TeamsContext, _a| {
        let tx = tx.clone();
        async move {
            assert!(ctx.client().has_credentials());
            assert_eq!(ctx.state().profile(), ctx.state().profile());
            assert_eq!(ctx.reference().conversation.id, CONVERSATION_ID);
            ctx.typing().await?;
            let sent = ctx.send(Activity::message("new")).await?;
            ctx.update(&sent.id, Activity::message("edited")).await?;
            ctx.delete(&sent.id).await?;
            tx.send("done".to_owned()).unwrap();
            Ok(())
        }
    }));
    post(&client, &message("go")).await.assert_status(200);
    assert_eq!(recv(&mut rx).await, "done");
    let base =
        format!("{SERVICE_URL}v3/conversations/19%3Atest-conversation%40thread.tacv2/activities");
    let got: Vec<_> = connector_requests(&t)
        .into_iter()
        .map(|r| (r.method, r.url.clone(), r.json()))
        .collect();
    assert_eq!(got[0].0, Method::Post);
    assert_eq!(got[0].1, base);
    assert_eq!(got[0].2["type"], "typing");
    assert_eq!(got[1].2["text"], "new");
    assert!(got[1].2.get("replyToId").is_none(), "send is not a reply");
    assert_eq!(got[2].0, Method::Put);
    assert_eq!(got[2].1, format!("{base}/1"));
    assert_eq!(got[2].2["id"], "1");
    assert_eq!(got[2].2["text"], "edited");
    assert_eq!(got[3].0, Method::Delete);
    assert_eq!(got[3].1, format!("{base}/1"));
}
