//! Request processing: verify, parse, check, dedup, dispatch, ack.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use autumn_web::AppState;
use autumn_web::reexports::axum::{
    body::{Body, to_bytes},
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

    /// Gives 503 when shutdown started after the route check.
    fn closing(&self) -> Option<Response> {
        if self.runner.tracker.is_closed() {
            self.metrics.request("shutting_down");
            return Some(status(StatusCode::SERVICE_UNAVAILABLE));
        }
        None
    }

    /// Handles `POST {base}/messages`.
    pub(crate) async fn messages(self: Arc<Self>, headers: &HeaderMap, body: Body) -> Response {
        // The JWT check needs no body. It runs before the body is read.
        let auth = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok());
        let token = match self.verifier.verify(auth).await {
            Ok(t) => t,
            Err(e) => return self.reject(&e),
        };
        let body = match to_bytes(body, self.config.max_body_bytes).await {
            Ok(b) => b,
            Err(e) => {
                let (outcome, code) = if too_large(e) {
                    ("too_large", StatusCode::PAYLOAD_TOO_LARGE)
                } else {
                    ("bad_request", StatusCode::BAD_REQUEST)
                };
                self.metrics.request(outcome);
                return status(code);
            }
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
        // The check above verifies that `serviceUrl` is set.
        let Some(reference) = activity.conversation_reference() else {
            self.metrics.request("bad_request");
            return status(StatusCode::BAD_REQUEST);
        };
        let ctx = TeamsContext::new(self.client.clone(), self.state.clone(), reference);
        let kind = match activity.kind.as_str() {
            types::MESSAGE => return self.message(ctx, activity),
            types::CONVERSATION_UPDATE => return self.conversation_update(ctx, activity),
            types::INVOKE => return self.invoke(ctx, activity).await,
            types::MESSAGE_UPDATE => Kind::MessageUpdate,
            types::MESSAGE_DELETE => Kind::MessageDelete,
            types::MESSAGE_REACTION => Kind::Reaction,
            types::INSTALLATION_UPDATE => Kind::InstallationUpdate,
            types::EVENT => Kind::Event,
            _ => return self.unhandled(),
        };
        let key = if kind == Kind::Event {
            activity.name.clone().unwrap_or_default()
        } else {
            String::new()
        };
        let Some(handler) = self.registry.background(kind, &key) else {
            return self.unhandled();
        };
        let jobs = vec![(kind, Arc::clone(handler))];
        self.run_once(ctx, activity, jobs, None)
    }

    /// Runs `jobs` for `activity` in a tracked task, after the dedup check.
    /// The ack does not wait. A failed run of the first job replies with
    /// `error_text` when `error_text` is set.
    ///
    /// The dedup check is in the task: a dropped request cannot leave a
    /// stored ID with no run.
    fn run_once(
        &self,
        ctx: TeamsContext,
        activity: Activity,
        jobs: Vec<(Kind, Handler<()>)>,
        error_text: Option<String>,
    ) -> Response {
        if let Some(r) = self.closing() {
            return r;
        }
        let dedup = Dedup {
            replay: Arc::clone(&self.replay),
            metrics: Arc::clone(&self.metrics),
            window: Duration::from_secs(self.config.dedup_window_secs),
            now: self.now_secs(),
        };
        let runner = self.runner.clone();
        self.runner.spawn_task(async move {
            if !dedup.fresh(&activity).await {
                return;
            }
            dedup.metrics.request("ok");
            let mut first = None;
            for (kind, handler) in jobs {
                let handle = runner.spawn(kind, handler, ctx.clone(), activity.clone());
                first.get_or_insert(handle);
            }
            let (Some(text), Some(handle)) = (error_text, first) else {
                return;
            };
            if matches!(handle.await, Ok(RunOutcome::Ok(()))) {
                return;
            }
            // No error detail goes to Teams.
            if let Err(e) = ctx.reply(Activity::message(text)).await {
                tracing::warn!(error = %e, "teams error reply failed");
            }
        });
        ok()
    }

    /// A message: a command handler by first word, else `on_message`. A
    /// failed run replies with `error_text`.
    fn message(&self, ctx: TeamsContext, activity: Activity) -> Response {
        let command = activity
            .command()
            .and_then(|(word, _)| self.registry.background(Kind::Command, &word))
            .map(|h| (Kind::Command, Arc::clone(h)));
        let Some(job) = command.or_else(|| {
            self.registry
                .background(Kind::Message, "")
                .map(|h| (Kind::Message, Arc::clone(h)))
        }) else {
            return self.unhandled();
        };
        let text = self.config.error_text.clone();
        self.run_once(ctx, activity, vec![job], Some(text))
    }

    /// Members added, members removed, and the general update handler.
    fn conversation_update(&self, ctx: TeamsContext, activity: Activity) -> Response {
        let mut kinds = Vec::new();
        if !activity.members_added.is_empty() {
            kinds.push(Kind::MembersAdded);
        }
        if !activity.members_removed.is_empty() {
            kinds.push(Kind::MembersRemoved);
        }
        kinds.push(Kind::ConversationUpdate);
        let jobs: Vec<(Kind, Handler<()>)> = kinds
            .into_iter()
            .filter_map(|k| self.registry.background(k, "").map(|h| (k, Arc::clone(h))))
            .collect();
        if jobs.is_empty() {
            return self.unhandled();
        }
        self.run_once(ctx, activity, jobs, None)
    }

    /// An invoke: a card action by verb, else an invoke by name. The reply
    /// is the HTTP reply. After `invoke_timeout_ms`, an empty 200.
    ///
    /// No dedup: Teams sends an invoke again only when it got no reply, and
    /// it needs a reply each time.
    async fn invoke(&self, ctx: TeamsContext, activity: Activity) -> Response {
        let name = activity.name.clone().unwrap_or_default();
        let is_card = name == ADAPTIVE_CARD_ACTION;
        let card = is_card
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
            // Universal Actions need HTTP 200, with the error in the body.
            if is_card {
                return invoke_reply(&self.card_error(400));
            }
            return status(StatusCode::NOT_IMPLEMENTED);
        };
        if let Some(r) = self.closing() {
            return r;
        }
        self.metrics.request("ok");
        let mut handle = self.runner.spawn(kind, Arc::clone(handler), ctx, activity);
        let limit = Duration::from_millis(self.config.invoke_timeout_ms);
        let failed = |is_card: bool| {
            // No error detail goes to Teams.
            if is_card {
                invoke_reply(&self.card_error(500))
            } else {
                status(StatusCode::INTERNAL_SERVER_ERROR)
            }
        };
        match tokio::time::timeout(limit, &mut handle).await {
            // A Universal Action ack is HTTP 200 whatever the handler says.
            Ok(Ok(RunOutcome::Ok(mut reply))) => {
                if is_card {
                    reply.status = 200;
                }
                invoke_reply(&reply)
            }
            Ok(_) => failed(is_card),
            Err(_) => {
                // Teams waits 5 s only. The handler continues. Teams does
                // not get its reply.
                self.metrics.late_ack();
                tracing::warn!("teams invoke reply is late; sent a fallback reply");
                if is_card { failed(true) } else { ok() }
            }
        }
    }

    fn card_error(&self, status: u16) -> InvokeResponse {
        InvokeResponse::card_error(status, self.config.error_text.clone())
    }
}

/// Time limit for one dedup check. After it, the handler runs: delivery is
/// at least once.
const DEDUP_TIMEOUT: Duration = Duration::from_secs(2);

/// What the dedup check needs, for a task.
struct Dedup {
    replay: Arc<dyn WebhookReplayStore>,
    metrics: Arc<TeamsMetrics>,
    window: Duration,
    now: u64,
}

impl Dedup {
    /// Returns `true` for a new activity. Drops a repeat ID in the same
    /// conversation. An activity with no ID is always new. A store error or
    /// a slow store gives `true`.
    async fn fresh(&self, activity: &Activity) -> bool {
        let (Some(id), Some(conv)) = (&activity.id, &activity.conversation) else {
            return true;
        };
        let key = dedup_key(&conv.id, id);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(self.now);
        let check = self.replay.check_and_insert(&key, now, self.window);
        match tokio::time::timeout(DEDUP_TIMEOUT, check).await {
            Ok(Ok(true)) => true,
            Ok(Ok(false)) => {
                self.metrics.request("duplicate");
                false
            }
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "teams dedup store failed; running the handler");
                true
            }
            Err(_) => {
                tracing::warn!("teams dedup store is slow; running the handler");
                true
            }
        }
    }
}

/// The dedup key. The length prefix keeps `a:b` + `c` apart from `a` + `b:c`.
fn dedup_key(conversation: &str, id: &str) -> String {
    format!("teams:activity:{}:{conversation}:{id}", conversation.len())
}

/// Returns `true` when the body read stopped at `max_body_bytes`. Other
/// errors (for example a client that went away) are not "too large".
fn too_large(e: autumn_web::reexports::axum::Error) -> bool {
    let inner = e.into_inner();
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(inner.as_ref());
    while let Some(err) = cur {
        if err.is::<http_body_util::LengthLimitError>() {
            return true;
        }
        cur = err.source();
    }
    false
}

/// The HTTP ack for an invoke result.
fn invoke_reply(reply: &InvokeResponse) -> Response {
    let code = StatusCode::from_u16(reply.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    reply
        .body
        .as_ref()
        .map_or_else(|| status(code), |b| json(code, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_keys_do_not_collide() {
        assert_ne!(dedup_key("a:b", "c"), dedup_key("a", "b:c"));
        assert_eq!(dedup_key("19:x", "1"), "teams:activity:4:19:x:1");
    }
}
