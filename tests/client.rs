//! `TeamsClient` against the fake transport.
#![allow(clippy::unwrap_used, clippy::expect_used)] // Test helpers fail loudly.

mod common;

use std::sync::Arc;
use std::time::Duration;

use autumn_plugin_teams::activity::{Activity, Attachment, ChannelAccount, ConversationParameters};
use autumn_plugin_teams::testing;
use autumn_plugin_teams::transport::{HttpReply, MEMORY_ACCESS_TOKEN, MemoryTransport, Method};
use autumn_plugin_teams::{TeamsClient, TeamsError, TeamsPlugin};
use autumn_web::AppState;
use autumn_web::time::TickingClock;
use common::*;
use serde_json::json;

const CONV: &str = "19:abc@thread.tacv2;messageid=123";
const CONV_ENC: &str = "19%3Aabc%40thread.tacv2%3Bmessageid%3D123";
const TOKEN_URL: &str = "/oauth2/v2.0/token";

fn client_from(p: TeamsPlugin) -> TeamsClient {
    p.start(&AppState::for_test()).unwrap().client()
}

fn client(t: &MemoryTransport) -> TeamsClient {
    client_from(plugin(t))
}

fn url(rest: &str) -> String {
    format!("{SERVICE_URL}v3/{rest}")
}

/// Connector requests with method and URL.
fn calls(t: &MemoryTransport) -> Vec<(Method, String)> {
    connector_requests(t)
        .into_iter()
        .map(|r| (r.method, r.url))
        .collect()
}

// ------------------------------------------------------------- operations (AC7)

#[tokio::test]
async fn activity_calls_hit_their_paths_with_the_app_token() {
    let t = MemoryTransport::new();
    t.respond("/activities", HttpReply::json(200, &json!({"id": "new-1"})));
    let c = client(&t);
    let card = Attachment::adaptive_card(json!({"type": "AdaptiveCard", "version": "1.5"}));
    let msg = Activity::message("hi").with_attachment(card);
    assert_eq!(
        c.send_to_conversation(SERVICE_URL, CONV, &msg)
            .await
            .unwrap()
            .id,
        "new-1"
    );
    assert_eq!(
        c.reply_to_activity(SERVICE_URL, CONV, "a1", &msg)
            .await
            .unwrap()
            .id,
        "new-1"
    );
    c.update_activity(SERVICE_URL, CONV, "a1", &Activity::message("edit"))
        .await
        .unwrap();
    c.delete_activity(SERVICE_URL, CONV, "a1").await.unwrap();
    assert_eq!(
        calls(&t),
        vec![
            (
                Method::Post,
                url(&format!("conversations/{CONV_ENC}/activities"))
            ),
            (
                Method::Post,
                url(&format!("conversations/{CONV_ENC}/activities/a1"))
            ),
            (
                Method::Put,
                url(&format!("conversations/{CONV_ENC}/activities/a1"))
            ),
            (
                Method::Delete,
                url(&format!("conversations/{CONV_ENC}/activities/a1"))
            ),
        ]
    );
    let reqs = connector_requests(&t);
    for r in &reqs {
        assert_eq!(
            r.header("authorization").unwrap(),
            format!("Bearer {MEMORY_ACCESS_TOKEN}")
        );
    }
    assert_eq!(
        reqs[0].header("content-type").unwrap(),
        "application/json; charset=utf-8"
    );
    let body = reqs[0].json();
    assert_eq!(body["type"], "message");
    assert_eq!(body["text"], "hi");
    assert_eq!(
        body["attachments"][0]["contentType"],
        "application/vnd.microsoft.card.adaptive"
    );
    assert_eq!(body["conversation"]["id"], CONV);
    assert_eq!(reqs[1].json()["replyToId"], "a1");
    assert!(reqs[3].body.is_empty());
    // One token for all calls.
    assert_eq!(t.requests_to(TOKEN_URL).len(), 1);
}

#[tokio::test]
async fn member_and_team_calls_are_typed() {
    let t = MemoryTransport::new();
    t.respond(
        "GET /members/29%3Aa",
        HttpReply::json(
            200,
            &json!({"id": "29:a", "name": "Ada", "email": "ada@x.example"}),
        ),
    );
    t.respond(
        "GET /members",
        HttpReply::json(
            200,
            &json!([{"id": "29:a"}, {"id": "29:b", "userRole": "guest"}]),
        ),
    );
    t.respond(
        "GET /teams/19%3Ateam/conversations",
        HttpReply::json(
            200,
            &json!({"conversations": [{"id": "19:general"}, {"id": "19:c2", "name": "Ops"}]}),
        ),
    );
    t.respond(
        "GET /teams/19%3Ateam",
        HttpReply::json(
            200,
            &json!({"id": "19:team", "name": "Team", "aadGroupId": "g1"}),
        ),
    );
    let c = client(&t);
    let members = c.get_members(SERVICE_URL, CONV).await.unwrap();
    assert_eq!(members.len(), 2);
    assert_eq!(members[1].user_role.as_deref(), Some("guest"));
    let m = c.get_member(SERVICE_URL, CONV, "29:a").await.unwrap();
    assert_eq!(m.email.as_deref(), Some("ada@x.example"));
    let team = c.team_details(SERVICE_URL, "19:team").await.unwrap();
    assert_eq!(team.aad_group_id.as_deref(), Some("g1"));
    let ch = c.team_channels(SERVICE_URL, "19:team").await.unwrap();
    assert_eq!(ch.len(), 2);
    assert_eq!(ch[0].name, None);
    assert_eq!(ch[1].name.as_deref(), Some("Ops"));
}

#[tokio::test]
async fn paged_members_follow_the_continuation_token() {
    let t = MemoryTransport::new();
    t.respond_seq(
        "GET /pagedmembers",
        vec![
            HttpReply::json(
                200,
                &json!({"members": [{"id": "1"}], "continuationToken": "a b/c"}),
            ),
            HttpReply::json(
                200,
                &json!({"members": [{"id": "2"}], "continuationToken": "d"}),
            ),
            HttpReply::json(200, &json!({"members": [{"id": "3"}]})),
        ],
    );
    let c = client(&t);
    let all = c.all_members(SERVICE_URL, CONV, 10).await.unwrap();
    assert_eq!(
        all.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["1", "2", "3"]
    );
    let urls = calls(&t);
    assert_eq!(urls.len(), 3);
    assert!(
        urls[0].1.ends_with("/pagedmembers?pageSize=500"),
        "{}",
        urls[0].1
    );
    assert!(
        urls[1]
            .1
            .ends_with("/pagedmembers?pageSize=500&continuationToken=a+b%2Fc"),
        "{}",
        urls[1].1
    );
    // One page.
    let page = c
        .get_paged_members(SERVICE_URL, CONV, Some(2), Some("x"))
        .await
        .unwrap();
    assert!(page.continuation_token.is_none());
    assert!(
        calls(&t)[3]
            .1
            .ends_with("pagedmembers?pageSize=2&continuationToken=x")
    );
}

#[tokio::test]
async fn all_members_stops_at_max_pages() {
    let t = MemoryTransport::new();
    t.respond(
        "GET /pagedmembers",
        HttpReply::json(
            200,
            &json!({"members": [{"id": "1"}], "continuationToken": "more"}),
        ),
    );
    let c = client(&t);
    let all = c.all_members(SERVICE_URL, CONV, 3).await.unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(calls(&t).len(), 3);
}

#[tokio::test]
async fn create_conversation_and_send_to_a_reference() {
    let t = MemoryTransport::new();
    t.respond(
        "POST /v3/conversations",
        HttpReply::json(200, &json!({"id": "a:new", "serviceUrl": SERVICE_URL})),
    );
    let c = client(&t);
    let params = ConversationParameters::personal(
        ChannelAccount::new(testing::BOT_ID, None),
        ChannelAccount::new("29:u", None),
        TENANT_ID,
    );
    let conv = c.create_conversation(SERVICE_URL, &params).await.unwrap();
    assert_eq!(conv.id, "a:new");
    let body = connector_requests(&t)[0].json();
    assert_eq!(body["members"][0]["id"], "29:u");
    assert_eq!(body["channelData"]["tenant"]["id"], TENANT_ID);
    assert_eq!(body["isGroup"], false);
    // A stored reference (serde round trip) works for a proactive message.
    let reference = testing::message("x").conversation_reference().unwrap();
    let stored = serde_json::to_string(&reference).unwrap();
    let reference = serde_json::from_str(&stored).unwrap();
    // Longer than "POST /v3/conversations", so this pattern wins.
    t.respond(
        "POST /v3/conversations/19%3Atest-conversation%40thread.tacv2/activities",
        HttpReply::json(201, &json!({"id": "p1"})),
    );
    let r = c
        .send_to(&reference, &Activity::message("later"))
        .await
        .unwrap();
    assert_eq!(r.id, "p1");
    let req = connector_requests(&t).pop().unwrap();
    assert_eq!(req.method, Method::Post);
    assert!(
        req.url
            .ends_with("/v3/conversations/19%3Atest-conversation%40thread.tacv2/activities")
    );
    let body = req.json();
    assert_eq!(body["from"]["id"], testing::BOT_ID);
    assert_eq!(body["conversation"]["id"], CONVERSATION_ID);
    assert_eq!(body["channelId"], "msteams");
    assert!(body.get("replyToId").is_none());
}

#[tokio::test]
async fn generic_call_and_empty_replies() {
    let t = MemoryTransport::new();
    t.respond("/v3/custom", HttpReply::text(204, ""));
    let c = client(&t);
    let v = c
        .call(
            "custom_op",
            Method::Get,
            SERVICE_URL,
            &["v3", "custom", "a/b"],
            &[("q", "1 2")],
            None,
        )
        .await
        .unwrap();
    assert!(v.is_null());
    assert_eq!(
        calls(&t)[0].1,
        format!("{SERVICE_URL}v3/custom/a%2Fb?q=1+2")
    );
    // A service URL with no trailing slash works too.
    c.call(
        "custom_op",
        Method::Delete,
        "https://smba.trafficmanager.net/emea",
        &["v3", "custom"],
        &[],
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        t.requests().last().unwrap().url,
        "https://smba.trafficmanager.net/emea/v3/custom"
    );
}

// ------------------------------------------------------------------ errors (AC7)

#[tokio::test]
async fn connector_error_gives_a_typed_api_error() {
    let t = MemoryTransport::new();
    t.respond(
        "/activities",
        HttpReply::json(
            403,
            &json!({"error": {"code": "BotNotInConversationRoster", "message": "The bot is not part of the conversation roster."}}),
        ),
    );
    let c = client(&t);
    let e = c
        .send_to_conversation(SERVICE_URL, CONV, &Activity::message("x"))
        .await
        .unwrap_err();
    assert_eq!(e.api_code(), Some("BotNotInConversationRoster"));
    let TeamsError::Api {
        operation,
        status,
        message,
        ..
    } = &e
    else {
        panic!("{e:?}");
    };
    assert_eq!(*operation, "send_to_conversation");
    assert_eq!(*status, 403);
    assert!(message.as_deref().unwrap().contains("roster"));
    // A non-JSON error body.
    t.respond("/members", HttpReply::text(404, "<html>nope</html>"));
    let e = c.get_members(SERVICE_URL, CONV).await.unwrap_err();
    assert_eq!(e.api_code(), Some("unknown"));
    // A 2xx body that is not the expected type.
    t.respond("/v3/teams/", HttpReply::json(200, &json!([1, 2])));
    let e = c.team_details(SERVICE_URL, "t").await.unwrap_err();
    assert!(matches!(e, TeamsError::Decode(_)), "{e:?}");
}

// ------------------------------------------------------------------- retry (AC9)

#[tokio::test(start_paused = true)]
async fn rate_limit_waits_retry_after_then_succeeds() {
    let t = MemoryTransport::new();
    t.respond_seq(
        "/activities",
        vec![
            HttpReply::text(429, "").retry_after(2),
            HttpReply::json(201, &json!({"id": "ok"})),
        ],
    );
    let p = plugin(&t);
    let metrics = p.metrics();
    let c = client_from(p);
    let start = tokio::time::Instant::now();
    let r = c
        .send_to_conversation(SERVICE_URL, CONV, &Activity::message("x"))
        .await
        .unwrap();
    assert_eq!(r.id, "ok");
    assert!(start.elapsed() >= Duration::from_secs(2));
    assert_eq!(metrics.api_retries("send_to_conversation"), 1);
    assert_eq!(metrics.api_calls("send_to_conversation", "ok"), 1);
}

#[tokio::test(start_paused = true)]
async fn rate_limit_over_cap_or_out_of_attempts_gives_rate_limited() {
    let t = MemoryTransport::new();
    t.respond("/activities", HttpReply::text(429, "").retry_after(31));
    let c = client(&t);
    let e = c
        .send_to_conversation(SERVICE_URL, CONV, &Activity::message("x"))
        .await
        .unwrap_err();
    assert!(
        matches!(
            e,
            TeamsError::RateLimited {
                retry_after_secs: Some(31)
            }
        ),
        "{e:?}"
    );
    assert_eq!(calls(&t).len(), 1);
    t.respond("/members", HttpReply::text(429, "").retry_after(1));
    let e = c.get_members(SERVICE_URL, CONV).await.unwrap_err();
    assert!(matches!(e, TeamsError::RateLimited { .. }), "{e:?}");
    // max_attempts = 3.
    assert_eq!(calls(&t).len(), 4);
}

#[tokio::test(start_paused = true)]
async fn server_error_retries_get_put_delete_but_not_post() {
    let t = MemoryTransport::new();
    t.respond_seq(
        "/activities",
        vec![
            HttpReply::text(503, ""),
            HttpReply::json(200, &json!({"id": "x"})),
        ],
    );
    let c = client(&t);
    let e = c
        .send_to_conversation(SERVICE_URL, CONV, &Activity::message("x"))
        .await
        .unwrap_err();
    assert_eq!(e.api_code(), Some("unknown"));
    assert_eq!(calls(&t).len(), 1, "a POST never repeats after a 5xx");
    for (i, m) in [Method::Put, Method::Delete, Method::Get]
        .into_iter()
        .enumerate()
    {
        t.respond_seq(
            "/activities",
            vec![
                HttpReply::text(500, ""),
                HttpReply::json(200, &json!({"id": "x"})),
            ],
        );
        c.call(
            "op",
            m,
            SERVICE_URL,
            &["v3", "conversations", "c", "activities"],
            &[],
            None,
        )
        .await
        .unwrap();
        assert_eq!(calls(&t).len(), 1 + 2 * (i + 1), "{m:?}");
    }
    // Bounded: max_attempts = 3.
    t.respond("/members", HttpReply::text(500, ""));
    assert!(c.get_members(SERVICE_URL, CONV).await.is_err());
    assert_eq!(t.requests_to("/members").len(), 3);
}

#[tokio::test(start_paused = true)]
async fn network_error_retries_only_idempotent_calls() {
    let t = MemoryTransport::new();
    let c = client(&t);
    // Get the token first, so the failures hit the Connector.
    c.check_credentials().await.unwrap();
    t.fail_next("/activities", 1);
    let e = c
        .send_to_conversation(SERVICE_URL, CONV, &Activity::message("x"))
        .await
        .unwrap_err();
    assert!(matches!(e, TeamsError::Transport(_)), "{e:?}");
    assert_eq!(t.requests_to("/activities").len(), 1);
    // A GET retries up to max_attempts = 3.
    t.fail_next("/members", 3);
    let e = c.get_members(SERVICE_URL, CONV).await.unwrap_err();
    assert!(matches!(e, TeamsError::Transport(_)), "{e:?}");
    assert_eq!(t.requests_to("/members").len(), 3);
    t.fail_next("/members", 2);
    t.respond("/members", HttpReply::json(200, &json!([])));
    assert!(c.get_members(SERVICE_URL, CONV).await.unwrap().is_empty());
}

// ------------------------------------------------------------ outbound auth (AC8)

#[tokio::test]
async fn token_request_is_client_credentials_for_the_tenant() {
    for (tenant, want) in [
        (
            None,
            "https://login.microsoftonline.com/botframework.com/oauth2/v2.0/token",
        ),
        (
            Some("contoso-tenant"),
            "https://login.microsoftonline.com/contoso-tenant/oauth2/v2.0/token",
        ),
    ] {
        let t = MemoryTransport::new();
        let mut p = plugin(&t);
        if let Some(id) = tenant {
            p = p.with_tenant_id(id);
        }
        let c = client_from(p);
        c.check_credentials().await.unwrap();
        let reqs = t.requests_to(TOKEN_URL);
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, Method::Post);
        assert_eq!(reqs[0].url, want);
        let form = reqs[0].form();
        assert_eq!(form["grant_type"], "client_credentials");
        assert_eq!(form["client_id"], APP_ID);
        assert_eq!(form["client_secret"], PASSWORD);
        assert_eq!(form["scope"], "https://api.botframework.com/.default");
    }
}

#[tokio::test]
async fn token_is_cached_until_the_refresh_margin() {
    let t = MemoryTransport::new();
    let clock = Arc::new(TickingClock::starting_at(now()));
    let c = client_from(plugin(&t).with_clock(clock.clone()));
    let m = Activity::message("x");
    let send = || c.send_to_conversation(SERVICE_URL, CONV, &m);
    send().await.unwrap();
    clock.advance(Duration::from_secs(3_299));
    send().await.unwrap();
    assert_eq!(t.requests_to(TOKEN_URL).len(), 1);
    // 3600 - 300 s margin: refresh.
    clock.advance(Duration::from_secs(1));
    send().await.unwrap();
    assert_eq!(t.requests_to(TOKEN_URL).len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_calls_share_one_token_request() {
    let t = MemoryTransport::new();
    t.set_latency(Duration::from_millis(30));
    let c = client(&t);
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let c = c.clone();
        tasks.push(tokio::spawn(async move {
            c.send_to_conversation(SERVICE_URL, CONV, &Activity::message("x"))
                .await
        }));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    assert_eq!(t.requests_to(TOKEN_URL).len(), 1);
}

#[tokio::test]
async fn connector_401_gets_a_new_token_and_retries_once() {
    let t = MemoryTransport::new();
    t.respond_seq(
        TOKEN_URL,
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
    t.respond_seq(
        "/activities",
        vec![
            HttpReply::text(401, ""),
            HttpReply::json(200, &json!({"id": "x"})),
        ],
    );
    let c = client(&t);
    // A POST is safe to repeat after a 401: the Connector did nothing.
    c.send_to_conversation(SERVICE_URL, CONV, &Activity::message("x"))
        .await
        .unwrap();
    let auths: Vec<_> = connector_requests(&t)
        .iter()
        .map(|r| r.header("authorization").unwrap())
        .collect();
    assert_eq!(auths, ["Bearer old", "Bearer new"]);
    // A second 401 stops: one refresh per call.
    t.respond("/activities", HttpReply::text(401, ""));
    let e = c
        .send_to_conversation(SERVICE_URL, CONV, &Activity::message("x"))
        .await
        .unwrap_err();
    assert!(matches!(e, TeamsError::Api { status: 401, .. }), "{e:?}");
    assert_eq!(t.requests_to(TOKEN_URL).len(), 3);
    assert_eq!(connector_requests(&t).len(), 4);
}

#[tokio::test(start_paused = true)]
async fn token_errors_give_a_code_and_never_the_secret() {
    let t = MemoryTransport::new();
    t.respond(
        TOKEN_URL,
        HttpReply::json(
            401,
            &json!({"error": "invalid_client", "error_description": "AADSTS7000215: Invalid client secret test-app-password-000"}),
        ),
    );
    let p = plugin(&t);
    let metrics = p.metrics();
    let c = client_from(p);
    let e = c
        .send_to_conversation(SERVICE_URL, CONV, &Activity::message("x"))
        .await
        .unwrap_err();
    assert!(
        matches!(&e, TeamsError::Token { code } if code == "invalid_client"),
        "{e:?}"
    );
    assert!(!e.to_string().contains(PASSWORD));
    assert!(!format!("{e:?}").contains(PASSWORD));
    assert!(connector_requests(&t).is_empty());
    assert_eq!(metrics.token_requests("error"), 1);
    // A 5xx from the token endpoint retries (a token request is safe to repeat).
    t.respond_seq(
        TOKEN_URL,
        vec![
            HttpReply::text(503, ""),
            HttpReply::json(
                200,
                &json!({"token_type": "Bearer", "expires_in": 3600, "access_token": "a"}),
            ),
        ],
    );
    c.check_credentials().await.unwrap();
    // A reply with no access token.
    let t = MemoryTransport::new();
    t.respond(
        TOKEN_URL,
        HttpReply::json(200, &json!({"token_type": "Bearer"})),
    );
    let e = client(&t).check_credentials().await.unwrap_err();
    assert!(
        matches!(&e, TeamsError::Token { code } if code == "decode"),
        "{e:?}"
    );
}

#[tokio::test]
async fn missing_password_gives_a_config_error_and_no_request() {
    let t = MemoryTransport::new();
    let mut cfg = config();
    "TEAMS_TEST_NO_SUCH_PASSWORD".clone_into(&mut cfg.app_password_env);
    let p = TeamsPlugin::with_config(cfg)
        .with_app_id(APP_ID)
        .with_transport(t.clone())
        .with_key_source(authority().key_source());
    let c = p
        .start_with_role(&AppState::for_test(), autumn_web::ProcessRole::Worker)
        .unwrap()
        .client();
    assert!(!c.has_credentials());
    let e = c
        .send_to_conversation(SERVICE_URL, CONV, &Activity::message("x"))
        .await
        .unwrap_err();
    assert!(matches!(e, TeamsError::Config(_)), "{e:?}");
    assert!(e.to_string().contains("TEAMS_TEST_NO_SUCH_PASSWORD"));
    assert!(t.requests().is_empty());
}

// ---------------------------------------------------------- serviceUrl (AC10)

#[tokio::test]
async fn token_goes_only_to_allowed_service_urls() {
    let t = MemoryTransport::new();
    let c = client(&t);
    for bad in [
        "http://smba.trafficmanager.net/teams/",
        "https://evil.example/teams/",
        "https://smba.trafficmanager.net.evil.example/",
        "https://user:pw@smba.trafficmanager.net/teams/",
        "https://smba.trafficmanager.net:8443/teams/",
        "https://169.254.169.254/",
        "file:///etc/passwd",
        "not a url",
        "",
    ] {
        let e = c
            .send_to_conversation(bad, CONV, &Activity::message("x"))
            .await
            .unwrap_err();
        assert!(matches!(e, TeamsError::BadServiceUrl), "{bad}: {e:?}");
    }
    assert!(t.requests().is_empty(), "no token request and no call");
    // Case does not matter, and a trailing dot is not a bypass.
    c.send_to_conversation(
        "https://SMBA.trafficmanager.net/teams/",
        CONV,
        &Activity::message("x"),
    )
    .await
    .unwrap();
    assert!(
        c.send_to_conversation(
            "https://smba.trafficmanager.net./teams/",
            CONV,
            &Activity::message("x")
        )
        .await
        .is_err()
    );
}

#[test]
fn debug_never_shows_secrets() {
    let t = MemoryTransport::new();
    let c = client(&t);
    let p = plugin(&t);
    for text in [format!("{c:?}"), format!("{p:?}")] {
        assert!(!text.contains(PASSWORD), "{text}");
    }
}
