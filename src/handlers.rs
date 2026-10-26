//! Handler types, the registry, and the task runner.

use std::collections::HashMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use autumn_web::AppState;
use futures::FutureExt as _;
use futures::future::BoxFuture;
use tokio_util::task::TaskTracker;

use crate::activity::{Activity, ConversationReference, InvokeResponse, ResourceResponse};
use crate::client::{TeamsClient, addressed};
use crate::error::{HandlerResult, TeamsError};
use crate::metrics::TeamsMetrics;

/// What a handler gets besides the activity.
#[derive(Clone, Debug)]
pub struct TeamsContext {
    client: TeamsClient,
    state: AppState,
    reference: ConversationReference,
}

impl TeamsContext {
    pub(crate) const fn new(
        client: TeamsClient,
        state: AppState,
        reference: ConversationReference,
    ) -> Self {
        Self {
            client,
            state,
            reference,
        }
    }

    /// The Connector client.
    #[must_use]
    pub const fn client(&self) -> &TeamsClient {
        &self.client
    }

    /// The app state.
    #[must_use]
    pub const fn state(&self) -> &AppState {
        &self.state
    }

    /// The conversation of the current activity. Store it to send a
    /// proactive message later.
    #[must_use]
    pub const fn reference(&self) -> &ConversationReference {
        &self.reference
    }

    /// Replies to the current activity. In a channel, the reply goes to the
    /// same thread.
    ///
    /// # Errors
    /// Same as [`TeamsClient::call`].
    pub async fn reply(&self, activity: Activity) -> Result<ResourceResponse, TeamsError> {
        let r = &self.reference;
        match r.activity_id.as_deref() {
            Some(id) => {
                self.client
                    .reply_to_activity(
                        &r.service_url,
                        &r.conversation.id,
                        id,
                        &addressed(&activity, r),
                    )
                    .await
            }
            None => self.send(activity).await,
        }
    }

    /// Sends to the conversation of the current activity, not as a reply.
    ///
    /// # Errors
    /// Same as [`TeamsClient::call`].
    pub async fn send(&self, activity: Activity) -> Result<ResourceResponse, TeamsError> {
        self.client.send_to(&self.reference, &activity).await
    }

    /// Sends a typing indicator.
    ///
    /// # Errors
    /// Same as [`TeamsClient::call`].
    pub async fn typing(&self) -> Result<(), TeamsError> {
        self.send(Activity::typing()).await.map(drop)
    }

    /// Updates an activity in this conversation.
    ///
    /// # Errors
    /// Same as [`TeamsClient::call`].
    pub async fn update(
        &self,
        activity_id: &str,
        activity: Activity,
    ) -> Result<ResourceResponse, TeamsError> {
        let r = &self.reference;
        self.client
            .update_activity(
                &r.service_url,
                &r.conversation.id,
                activity_id,
                &addressed(&activity, r),
            )
            .await
    }

    /// Deletes an activity in this conversation.
    ///
    /// # Errors
    /// Same as [`TeamsClient::call`].
    pub async fn delete(&self, activity_id: &str) -> Result<(), TeamsError> {
        let r = &self.reference;
        self.client
            .delete_activity(&r.service_url, &r.conversation.id, activity_id)
            .await
    }
}

/// A boxed handler.
pub(crate) type Handler<R> =
    Arc<dyn Fn(TeamsContext, Activity) -> BoxFuture<'static, HandlerResult<R>> + Send + Sync>;

/// Boxes a handler function.
pub(crate) fn boxed<R, F, Fut>(f: F) -> Handler<R>
where
    F: Fn(TeamsContext, Activity) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = HandlerResult<R>> + Send + 'static,
{
    Arc::new(move |ctx, activity| Box::pin(f(ctx, activity)))
}

/// Handler kinds. Each kind is also a metrics label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    Message,
    Command,
    MembersAdded,
    MembersRemoved,
    ConversationUpdate,
    Reaction,
    InstallationUpdate,
    Event,
    Invoke,
    CardAction,
}

impl Kind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Command => "command",
            Self::MembersAdded => "members_added",
            Self::MembersRemoved => "members_removed",
            Self::ConversationUpdate => "conversation_update",
            Self::Reaction => "reaction",
            Self::InstallationUpdate => "installation_update",
            Self::Event => "event",
            Self::Invoke => "invoke",
            Self::CardAction => "card_action",
        }
    }

    /// Kinds that take a key (a command word, a name, or a verb).
    const fn keyed(self) -> bool {
        matches!(
            self,
            Self::Command | Self::Event | Self::Invoke | Self::CardAction
        )
    }
}

/// One registration from the builder. `key` is empty for kinds with no key.
pub(crate) enum Registration {
    Background(Kind, String, Handler<()>),
    Invoke(Kind, String, Handler<InvokeResponse>),
}

impl Registration {
    /// Kind and key, for logs and errors.
    pub(crate) fn describe(&self) -> (Kind, &str) {
        match self {
            Self::Background(k, key, _) | Self::Invoke(k, key, _) => (*k, key),
        }
    }
}

/// Handlers by kind and key.
#[derive(Default)]
pub(crate) struct Registry {
    background: HashMap<(Kind, String), Handler<()>>,
    invokes: HashMap<(Kind, String), Handler<InvokeResponse>>,
}

impl Registry {
    /// Builds the registry. Command words are stored in lower case.
    ///
    /// # Errors
    /// Returns [`TeamsError::Config`] for an empty key or a key with a space
    /// where a key is needed, or a handler that is registered twice.
    pub(crate) fn build(regs: Vec<Registration>) -> Result<Self, TeamsError> {
        let mut r = Self::default();
        for reg in regs {
            let (kind, key) = reg.describe();
            let name = kind.as_str();
            if kind.keyed() && (key.trim().is_empty() || key.chars().any(char::is_whitespace)) {
                return Err(TeamsError::Config(format!(
                    "{name} key {key:?} is empty or has a space"
                )));
            }
            let key = if kind == Kind::Command {
                key.trim_start_matches('/').to_lowercase()
            } else {
                key.to_owned()
            };
            let dup = || {
                TeamsError::Config(if key.is_empty() {
                    format!("{name} handler is registered twice")
                } else {
                    format!("{name} {key} is registered twice")
                })
            };
            let taken = match reg {
                Registration::Background(_, _, h) => {
                    r.background.insert((kind, key.clone()), h).is_some()
                }
                Registration::Invoke(_, _, h) => r.invokes.insert((kind, key.clone()), h).is_some(),
            };
            if taken {
                return Err(dup());
            }
        }
        Ok(r)
    }

    /// The background handler for this kind and key.
    pub(crate) fn background(&self, kind: Kind, key: &str) -> Option<&Handler<()>> {
        self.background.get(&(kind, key.to_owned()))
    }

    /// The invoke handler for this kind and key.
    pub(crate) fn invoke(&self, kind: Kind, key: &str) -> Option<&Handler<InvokeResponse>> {
        self.invokes.get(&(kind, key.to_owned()))
    }

    /// Number of handlers.
    pub(crate) fn len(&self) -> usize {
        self.background.len() + self.invokes.len()
    }
}

/// How a handler run ended.
pub(crate) enum RunOutcome<R> {
    Ok(R),
    Failed,
    Panic,
}

/// Spawns handler runs. Counts them. Catches panics.
#[derive(Clone)]
pub(crate) struct Runner {
    pub tracker: TaskTracker,
    pub metrics: Arc<TeamsMetrics>,
}

impl Runner {
    /// Spawns `handler(ctx, activity)` as a tracked task.
    pub(crate) fn spawn<R>(
        &self,
        kind: Kind,
        handler: Handler<R>,
        ctx: TeamsContext,
        activity: Activity,
    ) -> tokio::task::JoinHandle<RunOutcome<R>>
    where
        R: Send + 'static,
    {
        let guard = InFlight::new(Arc::clone(&self.metrics));
        self.tracker.spawn(async move {
            // `catch_unwind` also catches a panic before the first poll.
            let out = AssertUnwindSafe(async move { handler(ctx, activity).await })
                .catch_unwind()
                .await;
            let metrics = Arc::clone(&guard.metrics);
            drop(guard);
            let label = kind.as_str();
            match out {
                Ok(Ok(r)) => {
                    metrics.handler(label, "ok");
                    RunOutcome::Ok(r)
                }
                Ok(Err(e)) => {
                    metrics.handler(label, "error");
                    tracing::warn!(kind = label, error = %e, "teams handler failed");
                    RunOutcome::Failed
                }
                Err(_) => {
                    metrics.handler(label, "panic");
                    tracing::error!(kind = label, "teams handler panicked");
                    RunOutcome::Panic
                }
            }
        })
    }

    /// Spawns a tracked task with no handler, for a follow-up reply.
    pub(crate) fn spawn_task(&self, fut: impl Future<Output = ()> + Send + 'static) {
        drop(self.tracker.spawn(fut));
    }
}

/// Counts one handler that runs. `drop` also runs when the task aborts.
struct InFlight {
    metrics: Arc<TeamsMetrics>,
}

impl InFlight {
    fn new(metrics: Arc<TeamsMetrics>) -> Self {
        metrics.in_flight_add(1);
        Self { metrics }
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.metrics.in_flight_add(-1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bg() -> Handler<()> {
        boxed(|_c, _a| async { Ok(()) })
    }

    fn inv() -> Handler<InvokeResponse> {
        boxed(|_c, _a| async { Ok(InvokeResponse::ok()) })
    }

    #[test]
    fn registry_rules() {
        let r = Registry::build(vec![
            Registration::Background(Kind::Command, "Deploy".into(), bg()),
            Registration::Background(Kind::Message, String::new(), bg()),
            Registration::Invoke(Kind::CardAction, "approve".into(), inv()),
        ])
        .unwrap();
        assert_eq!(r.len(), 3);
        // Command words are case-insensitive.
        assert!(r.background(Kind::Command, "deploy").is_some());
        assert!(r.background(Kind::Message, "").is_some());
        assert!(r.invoke(Kind::CardAction, "approve").is_some());
        assert!(r.invoke(Kind::Invoke, "approve").is_none());
        for bad in ["", "a b", " "] {
            let e = Registry::build(vec![Registration::Background(
                Kind::Command,
                bad.into(),
                bg(),
            )])
            .err()
            .unwrap();
            assert!(matches!(e, TeamsError::Config(_)), "{bad}");
        }
        let e = Registry::build(vec![
            Registration::Background(Kind::Command, "go".into(), bg()),
            Registration::Background(Kind::Command, "GO".into(), bg()),
        ])
        .err()
        .unwrap();
        assert!(e.to_string().contains("command go"), "{e}");
        let e = Registry::build(vec![
            Registration::Background(Kind::Message, String::new(), bg()),
            Registration::Background(Kind::Message, String::new(), bg()),
        ])
        .err()
        .unwrap();
        assert!(e.to_string().contains("message"), "{e}");
        // An invoke name can have a slash.
        Registry::build(vec![Registration::Invoke(
            Kind::Invoke,
            "task/fetch".into(),
            inv(),
        )])
        .unwrap();
    }
}
