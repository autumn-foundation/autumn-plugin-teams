//! Boots a real app (not `TestApp`) with only the Teams plugin.
//!
//! Regressions: autumn refuses to start with no `Route`, so the plugin must
//! give a real route. A `#[job]` must get the client, so the plugin must
//! start before job workers. Secrets come from env vars.
//!
//! The test runs this test binary again as a child process, with the env
//! vars set. So it changes no env in this process.
#![allow(clippy::unwrap_used, clippy::expect_used)] // Test helpers fail loudly.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use autumn_plugin_teams::testing::{self, TestAuthority};
use autumn_plugin_teams::{TeamsClient, TeamsPlugin};
use autumn_web::reexports::axum::{self, Router, routing::get};
use serde_json::json;

const CHILD_MARK: &str = "TEAMS_BOOT_TEST_CHILD";

/// The server. It runs only in the child process.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs only as the child of teams_only_app_boots_and_checks_tokens"]
async fn boot_child_server() {
    if std::env::var(CHILD_MARK).is_err() {
        return;
    }
    autumn_web::app()
        .plugin(TeamsPlugin::new())
        // autumn runs state initializers before job workers and startup
        // hooks. The client must be ready here, so a `#[job]` can use it.
        .state_initializer(|state| {
            if TeamsClient::from_state(state).is_none() {
                eprintln!("no TeamsClient before job workers start");
                std::process::exit(3);
            }
        })
        .run()
        .await;
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Serves `OpenID` metadata and the test key set. Returns the metadata URL.
async fn key_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let jwks = TestAuthority::new(testing::APP_ID).jwks();
    let meta = json!({ "jwks_uri": format!("{base}keys") });
    let app = Router::new()
        .route("/meta", get(move || async move { axum::Json(meta) }))
        .route("/keys", get(move || async move { axum::Json(jwks) }));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("{base}meta")
}

#[tokio::test(flavor = "multi_thread")]
async fn teams_only_app_boots_and_checks_tokens() {
    let metadata_url = key_server().await;
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let dir = std::env::temp_dir().join(format!("teams-boot-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut child = KillOnDrop(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "boot_child_server",
                "--include-ignored",
                "--nocapture",
            ])
            // A clean env and an empty dir: a developer's AUTUMN_* vars,
            // `.env`, or `autumn.toml` cannot change the result.
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("AUTUMN_MANIFEST_DIR", &dir)
            .current_dir(&dir)
            .env(CHILD_MARK, "1")
            .env("AUTUMN_SERVER__PORT", port.to_string())
            .env("AUTUMN_SERVER__HOST", "127.0.0.1")
            .env("TEAMS_APP_ID", testing::APP_ID)
            .env("TEAMS_APP_PASSWORD", "boot-password")
            .env("AUTUMN_TEAMS__OPENID_METADATA_URL", &metadata_url)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let activity = testing::message("hello");
    let body = serde_json::to_string(&activity).unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let url = format!("http://127.0.0.1:{port}/teams/messages");
    let send = |who: &TestAuthority| {
        let now = u64::try_from(autumn_web::reexports::chrono::Utc::now().timestamp()).unwrap();
        client
            .post(&url)
            .header("authorization", who.bearer(&activity, now))
            .header("content-type", "application/json")
            .body(body.clone())
            .send()
    };
    let good = TestAuthority::new(testing::APP_ID);
    // Wait for the app to boot.
    let start = Instant::now();
    let first = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            let mut err = String::new();
            if let Some(mut e) = child.0.stderr.take() {
                std::io::Read::read_to_string(&mut e, &mut err).unwrap();
            }
            panic!("the app stopped at boot ({status}):\n{err}");
        }
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "the app did not answer"
        );
        if let Ok(res) = send(&good).await {
            break res;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(first.status().as_u16(), 200);
    let rogue = TestAuthority::rogue(testing::APP_ID);
    assert_eq!(send(&rogue).await.unwrap().status().as_u16(), 401);
    let other_app = TestAuthority::new("another-app");
    assert_eq!(send(&other_app).await.unwrap().status().as_u16(), 401);
}
