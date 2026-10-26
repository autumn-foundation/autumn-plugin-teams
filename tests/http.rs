//! `ReqwestTransport` against a local fake Bot Framework: token endpoint,
//! `OpenID` metadata, keys, and the Connector.
#![allow(clippy::unwrap_used, clippy::expect_used)] // Test helpers fail loudly.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use autumn_plugin_teams::activity::Activity;
use autumn_plugin_teams::auth::{KeySource, OpenIdKeySource};
use autumn_plugin_teams::transport::{HttpRequest, HttpTransport, Method, ReqwestTransport};
use autumn_plugin_teams::{TeamsError, TeamsPlugin};
use autumn_web::AppState;
use autumn_web::reexports::axum::{
    self, Router,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use common::*;
use serde_json::json;

#[derive(Default, Clone)]
struct Seen {
    calls: Arc<Mutex<Vec<(HeaderMap, String)>>>,
}

async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{addr}/")
}

fn fake(seen: &Seen, base: Arc<Mutex<String>>) -> Router {
    let s = seen.clone();
    let jwks = authority().jwks();
    Router::new()
        .route(
            "/tenant-1/oauth2/v2.0/token",
            post(|body: String| async move {
                assert!(body.contains("grant_type=client_credentials"));
                axum::Json(json!({"token_type": "Bearer", "expires_in": 3600, "access_token": "real-token"}))
            }),
        )
        .route(
            "/meta",
            get(move || {
                let base = base.lock().unwrap().clone();
                async move { axum::Json(json!({"jwks_uri": format!("{base}keys")})) }
            }),
        )
        .route("/keys", get(move || async move { axum::Json(jwks) }))
        .route(
            "/teams/v3/conversations/{id}/activities",
            post(move |headers: HeaderMap, body: String| {
                let s = s.clone();
                async move {
                    let n = {
                        let mut calls = s.calls.lock().unwrap();
                        calls.push((headers, body));
                        calls.len()
                    };
                    if n == 1 {
                        (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "1")], "{}").into_response()
                    } else {
                        (StatusCode::CREATED, axum::Json(json!({"id": "a-1"}))).into_response()
                    }
                }
            }),
        )
}

#[tokio::test(flavor = "multi_thread")]
async fn real_http_round_trip_with_token_keys_and_429_retry() {
    let seen = Seen::default();
    let base_cell = Arc::new(Mutex::new(String::new()));
    let base = serve(fake(&seen, Arc::clone(&base_cell))).await;
    base_cell.lock().unwrap().clone_from(&base);
    let transport = Arc::new(ReqwestTransport::new(Duration::from_secs(5)).unwrap());

    // Keys from the OpenID metadata over real HTTP (loopback http is allowed
    // for tests only).
    let keys = OpenIdKeySource::new(transport.clone(), format!("{base}meta"))
        .fetch()
        .await
        .unwrap();
    assert_eq!(keys.len(), 1);

    let mut c = config();
    c.login_base_url.clone_from(&base);
    c.openid_metadata_url = format!("{base}meta");
    c.service_url_hosts = vec!["127.0.0.1".to_owned()];
    let rt = TeamsPlugin::with_config(c)
        .with_app_id(APP_ID)
        .with_app_password(PASSWORD)
        .with_tenant_id("tenant-1")
        .with_transport_arc(transport.clone())
        .start(&AppState::for_test())
        .unwrap();
    let service = format!("{base}teams/");
    let r = rt
        .client()
        .send_to_conversation(&service, "19:a@thread", &Activity::message("hi"))
        .await
        .unwrap();
    assert_eq!(r.id, "a-1");
    let calls = seen.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2, "one 429, then one retry");
    assert_eq!(calls[1].0["authorization"], "Bearer real-token");
    assert!(calls[1].1.contains("\"text\":\"hi\""));
    assert_eq!(rt.metrics().api_retries("send_to_conversation"), 1);

    // A closed port gives a transport error.
    let e = transport
        .send(HttpRequest::new(Method::Get, "http://127.0.0.1:9/"))
        .await
        .unwrap_err();
    assert!(!e.0.is_empty());
    let e = rt
        .client()
        .send_to_conversation("http://127.0.0.1:9/", "c", &Activity::message("x"))
        .await
        .unwrap_err();
    assert!(matches!(e, TeamsError::Transport(_)), "{e:?}");
}
