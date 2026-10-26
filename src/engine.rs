//! Request processing: verify, parse, check, dedup, dispatch, ack.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use autumn_web::AppState;
use autumn_web::reexports::axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode, header::AUTHORIZATION},
    response::Response,
};
use autumn_web::time::{ClockSource, clock_unix_secs};
use autumn_web::webhook::WebhookReplayStore;

use crate::activity::{ADAPTIVE_CARD_ACTION, Activity, InvokeResponse, types};
use crate::auth::{AuthError, Verifier, check_activity};
use crate::client::TeamsClient;
use crate::config::TeamsConfig;
use crate::handlers::{Handler, Kind, Registry, RunOutcome, Runner, TeamsContext};
use crate::metrics::TeamsMetrics;
use crate::routes::{json, status};

/// The started plugin core. The route calls it.
pub(crate) struct Engine {
    pub config: TeamsConfig,
    pub verifier: Verifier,
    pub registry: Registry,
    pub client: TeamsClient,
    pub metrics: Arc<TeamsMetrics>,
    pub clock: Arc<dyn ClockSource>,
    pub replay: Arc<dyn WebhookReplayStore>,
    pub runner: Runner,
    pub state: AppState,
}

fn ok() -> Response {
    status(StatusCode::OK)
}

impl Engine {
    fn now_secs(&self) -> u64 {
        clock_unix_secs(self.clock.as_ref())
    }

    fn reject(&self, e: &AuthError) -> Response {
        self.metrics.request(e.label());
        tracing::debug!(error = %e, "teams request rejected");
        status(StatusCode::from_u16(e.status()).unwrap_or(StatusCode::UNAUTHORIZED))
    }

    fn unhandled(&self) -> Response {
        self.metrics.request("unhandled");
        ok()
    }

    /// Handles `POST {base}/messages`.
    pub(crate) async fn messages(&self, headers: &HeaderMap, body: Bytes) -> Response {
        // The token check needs no body. It runs before any parse.
        let auth = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok());
        let token = match self.verifier.verify(auth, self.now_secs()).await {
            Ok(t) => t,
            Err(e) => return self.reject(&e),
        };
        let Ok(activity) = serde_json::from_slice::<Activity>(&body) else {
            self.metrics.request("bad_request");
            return status(StatusCode::BAD_REQUEST);
        };
        let cfg = &self.config;
        if let Err(e) = check_activity(
            &token,
            &activity,
            &cfg.allowed_channels,
            &cfg.allowed_tenants,
        ) {
            return self.reject(&e);
        }
        // The check above makes sure that `serviceUrl` is set.
        let Some(reference) = activity.conversation_reference() else {
            self.metrics.request("bad_request");
            return status(StatusCode::BAD_REQUEST);
        };
        let ctx = TeamsContext::new(self.client.clone(), self.state.clone(), reference);
        match activity.kind.as_str() {
            types::MESSAGE => self.message(ctx, activity).await,
            types::CONVERSATION_UPDATE => self.conversation_update(ctx, activity).await,
            types::MESSAGE_REACTION => self.background(Kind::Reaction, "", ctx, activity).await,
            types::INSTALLATION_UPDATE => {
                self.background(Kind::InstallationUpdate, "", ctx, activity)
                    .await
            }
            types::EVENT => {
                let name = activity.name.clone().unwrap_or_default();
                self.background(Kind::Event, &name, ctx, activity).await
            }
            types::INVOKE => self.invoke(ctx, activity).await,
            _ => self.unhandled(),
        }
    }

    /// Returns `true` for a new activity. Drops a repeat ID in the same
    /// conversation. An activity with no ID is always new. A store error
    /// gives `true`: delivery is at least once.
    async fn fresh(&self, activity: &Activity) -> bool {
        let (Some(id), Some(conv)) = (&activity.id, &activity.conversation) else {
            return true;
        };
        let key = format!("teams:activity:{}:{id}", conv.id);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(self.now_secs());
        let window = Duration::from_secs(self.config.dedup_window_secs);
        match self.replay.check_and_insert(&key, now, window).await {
            Ok(fresh) => {
                if !fresh {
                    self.metrics.request("duplicate");
                }
                fresh
            }
            Err(e) => {
                tracing::warn!(error = %e, "teams dedup store failed; running the handler");
                true
            }
        }
    }

    /// Runs one background handler of `kind` and `key`, if any.
    async fn background(
        &self,
        kind: Kind,
        key: &str,
        ctx: TeamsContext,
        activity: Activity,
    ) -> Response {
        let Some(handler) = self.registry.background(kind, key) else {
            return self.unhandled();
        };
        if !self.fresh(&activity).await {
            return ok();
        }
        self.metrics.request("ok");
        drop(self.runner.spawn(kind, Arc::clone(handler), ctx, activity));
        ok()
    }

    /// A message: a command handler by first word, else `on_message`. A
    /// failed run replies with `error_text`.
    async fn message(&self, ctx: TeamsContext, activity: Activity) -> Response {
        let command = activity
            .command()
            .and_then(|(word, _)| self.registry.background(Kind::Command, &word))
            .map(|h| (Kind::Command, h));
        let Some((kind, handler)) = command.or_else(|| {
            self.registry
                .background(Kind::Message, "")
                .map(|h| (Kind::Message, h))
        }) else {
            return self.unhandled();
        };
        if !self.fresh(&activity).await {
            return ok();
        }
        self.metrics.request("ok");
        let handle = self
            .runner
            .spawn(kind, Arc::clone(handler), ctx.clone(), activity);
        let error_text = self.config.error_text.clone();
        self.runner.spawn_task(async move {
            if matches!(handle.await, Ok(RunOutcome::Ok(()))) {
                return;
            }
            // No error detail goes to Teams.
            if let Err(e) = ctx.reply(Activity::message(error_text)).await {
                tracing::warn!(error = %e, "teams error reply failed");
            }
        });
        ok()
    }

    /// Members added, members removed, and the general update handler.
    async fn conversation_update(&self, ctx: TeamsContext, activity: Activity) -> Response {
        let mut kinds = Vec::new();
        if !activity.members_added.is_empty() {
            kinds.push(Kind::MembersAdded);
        }
        if !activity.members_removed.is_empty() {
            kinds.push(Kind::MembersRemoved);
        }
        kinds.push(Kind::ConversationUpdate);
        let handlers: Vec<(Kind, Handler<()>)> = kinds
            .into_iter()
            .filter_map(|k| self.registry.background(k, "").map(|h| (k, Arc::clone(h))))
            .collect();
        if handlers.is_empty() {
            return self.unhandled();
        }
        if !self.fresh(&activity).await {
            return ok();
        }
        self.metrics.request("ok");
        for (kind, handler) in handlers {
            drop(
                self.runner
                    .spawn(kind, handler, ctx.clone(), activity.clone()),
            );
        }
        ok()
    }

    /// An invoke: a card action by verb, else an invoke by name. The reply
    /// is the HTTP reply. After `invoke_timeout_ms`, an empty 200.
    async fn invoke(&self, ctx: TeamsContext, activity: Activity) -> Response {
        let name = activity.name.clone().unwrap_or_default();
        let card = (name == ADAPTIVE_CARD_ACTION)
            .then(|| activity.card_action())
            .flatten()
            .and_then(|a| self.registry.invoke(Kind::CardAction, &a.verb))
            .map(|h| (Kind::CardAction, h));
        let Some((kind, handler)) = card.or_else(|| {
            self.registry
                .invoke(Kind::Invoke, &name)
                .map(|h| (Kind::Invoke, h))
        }) else {
            self.metrics.request("unhandled");
            return status(StatusCode::NOT_IMPLEMENTED);
        };
        if !self.fresh(&activity).await {
            return ok();
        }
        self.metrics.request("ok");
        let mut handle = self.runner.spawn(kind, Arc::clone(handler), ctx, activity);
        let limit = Duration::from_millis(self.config.invoke_timeout_ms);
        match tokio::time::timeout(limit, &mut handle).await {
            Ok(Ok(RunOutcome::Ok(reply))) => invoke_reply(&reply),
            // An error or a panic: no detail goes to Teams.
            Ok(_) => status(StatusCode::INTERNAL_SERVER_ERROR),
            Err(_) => {
                // Teams waits 5 s only. The handler goes on; its reply is lost.
                self.metrics.late_ack();
                tracing::warn!("teams invoke reply is late; sent an empty 200");
                ok()
            }
        }
    }
}

/// The HTTP reply for an invoke result.
fn invoke_reply(reply: &InvokeResponse) -> Response {
    let code = StatusCode::from_u16(reply.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    reply
        .body
        .as_ref()
        .map_or_else(|| status(code), |b| json(code, b))
}
