//! `TeamsPlugin` and the started runtime.

use std::borrow::Cow;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use autumn_web::actuator::{HealthIndicator, MetricsSource};
use autumn_web::app::AppBuilder;
use autumn_web::config::AutumnConfig;
use autumn_web::plugin::Plugin;
use autumn_web::plugin_contract::PluginContract;
use autumn_web::reexports::axum::Router;
use autumn_web::time::{ClockSource, SystemClock};
use autumn_web::webhook::{InMemoryWebhookReplayStore, WebhookReplayStore};
use autumn_web::{AppState, AutumnError, ProcessRole};
use tokio_util::task::TaskTracker;

use crate::activity::{Activity, InvokeResponse};
use crate::auth::{KeySource, OpenIdKeySource, Verifier, VerifierRules};
use crate::client::TeamsClient;
use crate::config::{SECTION, TeamsConfig, env_var};
use crate::engine::Engine;
use crate::error::{HandlerResult, TeamsError};
use crate::handlers::{Kind, Registration, Registry, Runner, TeamsContext, boxed};
use crate::health::TeamsHealth;
use crate::metrics::TeamsMetrics;
use crate::routes::{self, EngineCell, MESSAGES};
use crate::token::{Credentials, TokenProvider};
use crate::transport::{HttpTransport, ReqwestTransport};

/// Plugin name for duplicate detection.
pub const PLUGIN_NAME: &str = "autumn-plugin-teams";
/// Default base path of the route.
pub const DEFAULT_BASE_PATH: &str = "/teams";

/// Microsoft Teams plugin.
///
/// ```rust,ignore
/// autumn_web::app()
///     .plugin(
///         TeamsPlugin::new()
///             .command("deploy", deploy)
///             .card_action("approve", approve),
///     )
///     .run()
///     .await;
/// ```
pub struct TeamsPlugin {
    config: Option<TeamsConfig>,
    base_path: String,
    readiness: bool,
    app_id: Option<String>,
    app_password: Option<String>,
    tenant_id: Option<String>,
    transport: Option<Arc<dyn HttpTransport>>,
    clock: Option<Arc<dyn ClockSource>>,
    keys: Option<Arc<dyn KeySource>>,
    replay: Option<Arc<dyn WebhookReplayStore>>,
    registrations: Vec<Registration>,
    metrics: Arc<TeamsMetrics>,
}

impl std::fmt::Debug for TeamsPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeamsPlugin")
            .field("base_path", &self.base_path)
            .field(
                "handlers",
                &self
                    .registrations
                    .iter()
                    .map(|r| {
                        let (k, key) = r.describe();
                        (k.as_str(), key.to_owned())
                    })
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl Default for TeamsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

macro_rules! background {
    ($(#[$doc:meta])* $name:ident, $kind:expr) => {
        $(#[$doc])*
        #[must_use]
        pub fn $name<F, Fut>(self, handler: F) -> Self
        where
            F: Fn(TeamsContext, Activity) -> Fut + Send + Sync + 'static,
            Fut: Future<Output = HandlerResult<()>> + Send + 'static,
        {
            self.register(Registration::Background($kind, String::new(), boxed(handler)))
        }
    };
}

impl TeamsPlugin {
    /// Makes the plugin. It reads `[teams]` at startup.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: None,
            base_path: DEFAULT_BASE_PATH.to_owned(),
            readiness: false,
            app_id: None,
            app_password: None,
            tenant_id: None,
            transport: None,
            clock: None,
            keys: None,
            replay: None,
            registrations: Vec::new(),
            metrics: Arc::new(TeamsMetrics::new()),
        }
    }

    /// Makes the plugin with this config. It reads no files.
    #[must_use]
    pub fn with_config(config: TeamsConfig) -> Self {
        Self {
            config: Some(config),
            ..Self::new()
        }
    }

    /// Sets the base path of the route. Default: `/teams`, so the route is
    /// `/teams/messages`.
    ///
    /// autumn mounts routes before it loads config. So this is a builder
    /// setting, not a config key.
    #[must_use]
    pub fn base_path(mut self, path: &str) -> Self {
        self.base_path = normalize_base(path);
        self
    }

    /// Puts the health indicator in `/ready` too. Default: only in
    /// `/actuator/health`.
    #[must_use]
    pub const fn readiness(mut self, on: bool) -> Self {
        self.readiness = on;
        self
    }

    /// Uses this app ID, not the env var.
    #[must_use]
    pub fn with_app_id(mut self, app_id: impl Into<String>) -> Self {
        self.app_id = Some(app_id.into());
        self
    }

    /// Uses this app password, not the env var.
    #[must_use]
    pub fn with_app_password(mut self, password: impl Into<String>) -> Self {
        self.app_password = Some(password.into());
        self
    }

    /// Uses this tenant ID (a single-tenant bot), not the env var.
    #[must_use]
    pub fn with_tenant_id(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_id = Some(tenant_id.into());
        self
    }

    /// Uses this transport. Default: [`crate::transport::ReqwestTransport`].
    #[must_use]
    pub fn with_transport(self, transport: impl HttpTransport) -> Self {
        self.with_transport_arc(Arc::new(transport))
    }

    /// Uses this shared transport.
    #[must_use]
    pub fn with_transport_arc(mut self, transport: Arc<dyn HttpTransport>) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Uses this clock for token times. Default: the system clock.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn ClockSource>) -> Self {
        self.clock = Some(clock);
        self
    }

    /// Uses this key source for inbound tokens. Default: the `OpenID`
    /// metadata at `openid_metadata_url`.
    #[must_use]
    pub fn with_key_source(mut self, keys: impl KeySource) -> Self {
        self.keys = Some(Arc::new(keys));
        self
    }

    /// Uses this store to drop repeat activities. Default: in memory, per
    /// process. Use a Redis store for many replicas.
    #[must_use]
    pub fn with_replay_store(mut self, store: Arc<dyn WebhookReplayStore>) -> Self {
        self.replay = Some(store);
        self
    }

    fn register(mut self, reg: Registration) -> Self {
        self.registrations.push(reg);
        self
    }

    background!(
        /// Runs `handler` for a message with no command handler, after the
        /// ack. A card `Action.Submit` also comes here, with `value` set.
        on_message,
        Kind::Message
    );

    /// Runs `handler` for a message whose first word is `word`, after the
    /// ack. The match ignores the bot mention, a leading `/`, and case.
    #[must_use]
    pub fn command<F, Fut>(self, word: &str, handler: F) -> Self
    where
        F: Fn(TeamsContext, Activity) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HandlerResult<()>> + Send + 'static,
    {
        self.register(Registration::Background(
            Kind::Command,
            word.to_owned(),
            boxed(handler),
        ))
    }

    background!(
        /// Runs `handler` for a `messageUpdate` (a user edited a message).
        on_message_update,
        Kind::MessageUpdate
    );

    background!(
        /// Runs `handler` for a `messageDelete` (a user deleted a message).
        on_message_delete,
        Kind::MessageDelete
    );

    background!(
        /// Runs `handler` for a `conversationUpdate` with `membersAdded`.
        on_members_added,
        Kind::MembersAdded
    );

    background!(
        /// Runs `handler` for a `conversationUpdate` with `membersRemoved`.
        on_members_removed,
        Kind::MembersRemoved
    );

    background!(
        /// Runs `handler` for each `conversationUpdate`, for example a
        /// channel rename. See [`Activity::event_type`].
        on_conversation_update,
        Kind::ConversationUpdate
    );

    background!(
        /// Runs `handler` for a `messageReaction`.
        on_reaction,
        Kind::Reaction
    );

    background!(
        /// Runs `handler` for an `installationUpdate` (app added or removed).
        on_installation_update,
        Kind::InstallationUpdate
    );

    /// Runs `handler` for an `event` activity with this `name`, after the ack.
    #[must_use]
    pub fn on_event<F, Fut>(self, name: &str, handler: F) -> Self
    where
        F: Fn(TeamsContext, Activity) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HandlerResult<()>> + Send + 'static,
    {
        self.register(Registration::Background(
            Kind::Event,
            name.to_owned(),
            boxed(handler),
        ))
    }

    /// Runs `handler` for an invoke with this `name`, for example
    /// `task/fetch` or `composeExtension/query`. Its reply is the HTTP reply.
    #[must_use]
    pub fn invoke<F, Fut>(self, name: &str, handler: F) -> Self
    where
        F: Fn(TeamsContext, Activity) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HandlerResult<InvokeResponse>> + Send + 'static,
    {
        self.register(Registration::Invoke(
            Kind::Invoke,
            name.to_owned(),
            boxed(handler),
        ))
    }

    /// Runs `handler` for an Adaptive Card `Action.Execute` with this verb.
    /// Its reply is the HTTP reply. See [`InvokeResponse::card`].
    #[must_use]
    pub fn card_action<F, Fut>(self, verb: &str, handler: F) -> Self
    where
        F: Fn(TeamsContext, Activity) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HandlerResult<InvokeResponse>> + Send + 'static,
    {
        self.register(Registration::Invoke(
            Kind::CardAction,
            verb.to_owned(),
            boxed(handler),
        ))
    }

    /// The metrics. They are shared with the started runtime.
    #[must_use]
    pub fn metrics(&self) -> Arc<TeamsMetrics> {
        Arc::clone(&self.metrics)
    }

    /// Starts the plugin outside an app build. Use it in tests, with
    /// [`TeamsRuntime::router`]. [`Plugin::build`] does the same at startup.
    ///
    /// # Errors
    /// Returns a config error.
    pub fn start(self, state: &AppState) -> Result<TeamsRuntime, TeamsError> {
        let role = state.role();
        self.start_with_role(state, role)
    }

    /// Starts the plugin as if the process has `role`. A role that serves no
    /// HTTP (`worker`) needs no app ID check and no CSRF exemption.
    ///
    /// # Errors
    /// Same as [`Self::start`].
    pub fn start_with_role(
        self,
        state: &AppState,
        role: ProcessRole,
    ) -> Result<TeamsRuntime, TeamsError> {
        let health = Arc::new(TeamsHealth::new(self.readiness));
        self.start_inner(state, role, health)
    }

    #[allow(clippy::too_many_lines)] // One linear setup sequence.
    fn start_inner(
        self,
        state: &AppState,
        role: ProcessRole,
        health: Arc<TeamsHealth>,
    ) -> Result<TeamsRuntime, TeamsError> {
        let config = match self.config {
            Some(c) => c,
            // autumn sets the state profile from the config; "default" means none.
            None => TeamsConfig::load(Some(state.profile()).filter(|p| *p != "default"))?,
        };
        config.validate()?;
        // A worker serves no route, so it needs no inbound checks.
        let inbound = role.serves_http();
        if inbound {
            check_security(&state.config(), &self.base_path)?;
        }
        let registry = Registry::build(self.registrations)?;
        let secret = |given: Option<String>, env: &str| {
            given
                .or_else(|| env_var(env))
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let app_id = secret(self.app_id, &config.app_id_env);
        let password = secret(self.app_password, &config.app_password_env);
        let tenant_id = secret(self.tenant_id, &config.tenant_id_env);
        let missing =
            |what: &str, env: &str| TeamsError::Config(format!("no {what}; set the env var {env}"));
        if inbound {
            if app_id.is_none() {
                return Err(missing("app ID", &config.app_id_env));
            }
            if password.is_none() {
                return Err(missing("app password", &config.app_password_env));
            }
        } else {
            if app_id.is_none() {
                tracing::warn!(env = %config.app_id_env, "teams: no app ID; Connector calls fail");
            }
            if password.is_none() {
                tracing::warn!(env = %config.app_password_env, "teams: no app password; Connector calls fail");
            }
        }
        let app_id = app_id.unwrap_or_default();
        let transport: Arc<dyn HttpTransport> = match self.transport {
            Some(t) => t,
            None => Arc::new(
                ReqwestTransport::new(Duration::from_millis(config.api.timeout_ms))
                    .map_err(|e| TeamsError::Config(e.0))?,
            ),
        };
        let clock = self.clock.unwrap_or_else(|| Arc::new(SystemClock));
        let tokens = Arc::new(TokenProvider::new(
            Arc::clone(&transport),
            Credentials {
                app_id: app_id.clone(),
                password,
                password_env: config.app_password_env.clone(),
                token_url: config.token_url(tenant_id.as_deref()),
                scope: config.oauth_scope.clone(),
            },
            Arc::clone(&clock),
            Arc::clone(&self.metrics),
            config.api.rule(),
        ));
        let client = TeamsClient::new(
            Arc::clone(&transport),
            config.clone(),
            tokens,
            Arc::clone(&self.metrics),
        );
        health.set(
            client.clone(),
            Duration::from_secs(config.health.cache_secs),
        );
        state.insert_extension(client.clone());
        let keys = self.keys.unwrap_or_else(|| {
            Arc::new(OpenIdKeySource::new(
                Arc::clone(&transport),
                config.openid_metadata_url.clone(),
            ))
        });
        let verifier = Verifier::new(
            VerifierRules {
                app_id,
                issuers: config.issuers.clone(),
                clock_skew_secs: config.clock_skew_secs,
                key_cache_secs: config.keys.cache_secs,
                key_min_refresh_secs: config.keys.min_refresh_secs,
            },
            keys,
            Arc::clone(&self.metrics),
            Arc::clone(&clock),
        );
        tracing::info!(
            base_path = %self.base_path,
            handlers = registry.len(),
            single_tenant = tenant_id.is_some(),
            "teams started"
        );
        let engine = Engine {
            verifier,
            registry,
            client,
            metrics: Arc::clone(&self.metrics),
            clock,
            replay: self
                .replay
                .unwrap_or_else(|| Arc::new(InMemoryWebhookReplayStore::default())),
            runner: Runner {
                tracker: TaskTracker::new(),
                metrics: Arc::clone(&self.metrics),
            },
            state: state.clone(),
            config,
        };
        Ok(TeamsRuntime {
            engine: Arc::new(engine),
            health,
        })
    }
}

/// `"/teams/"` and `"teams"` become `"/teams"`. `"/"` becomes `""` (the root).
fn normalize_base(path: &str) -> String {
    let trimmed = path.trim().trim_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("/{trimmed}")
    }
}

/// The full path of the route.
fn route_path(base: &str) -> String {
    format!("{base}{MESSAGES}")
}

/// Same rule as autumn: an exact match, or a `/`-delimited subtree.
fn is_exempt(path: &str, exempt: &[String]) -> bool {
    exempt.iter().any(|p| {
        let p = p.as_str();
        path == p
            || (path.starts_with(p)
                && (p.ends_with('/') || path.as_bytes().get(p.len()) == Some(&b'/')))
    })
}

/// Fails when CSRF would block the route. A plugin cannot add an exemption,
/// so the error gives the config fix. CAPTCHA checks only form bodies, and
/// the Connector sends JSON.
fn check_security(cfg: &AutumnConfig, base: &str) -> Result<(), TeamsError> {
    let path = route_path(base);
    // At the root, name the exact path: "/" would exempt the whole app.
    let fix = if base.is_empty() { &path } else { base };
    let mut exempt = cfg.security.csrf.exempt_paths.clone();
    exempt.extend(
        cfg.security
            .webhooks
            .endpoints
            .iter()
            .map(|e| e.path.clone()),
    );
    if cfg.security.csrf.enabled && !is_exempt(&path, &exempt) {
        return Err(TeamsError::Config(format!(
            "CSRF protection blocks {path}. Teams sends no CSRF token. \
             Add [\"{fix}\"] to [security.csrf] exempt_paths. \
             The Bot Framework token check protects this route."
        )));
    }
    Ok(())
}

/// A started plugin.
pub struct TeamsRuntime {
    engine: Arc<Engine>,
    health: Arc<TeamsHealth>,
}

impl std::fmt::Debug for TeamsRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeamsRuntime")
            .field("in_flight", &self.in_flight())
            .finish_non_exhaustive()
    }
}

impl TeamsRuntime {
    /// The Connector client.
    #[must_use]
    pub fn client(&self) -> TeamsClient {
        self.engine.client.clone()
    }

    /// The metrics.
    #[must_use]
    pub fn metrics(&self) -> Arc<TeamsMetrics> {
        Arc::clone(&self.engine.metrics)
    }

    /// The health indicator.
    #[must_use]
    pub fn health(&self) -> Arc<TeamsHealth> {
        Arc::clone(&self.health)
    }

    /// Handlers that run now. Follow-up tasks do not count.
    #[must_use]
    pub fn in_flight(&self) -> u64 {
        self.engine.metrics.in_flight()
    }

    /// The route, relative to the base path. Use it in tests, or to mount
    /// the route yourself.
    pub fn router(&self) -> Router {
        let cell: EngineCell = Arc::new(OnceLock::new());
        let _ = cell.set(Arc::clone(&self.engine));
        routes::router(&cell)
    }

    /// Stops new handler runs and waits for running handlers, up to
    /// `drain_timeout_secs`. After this, the route replies 503.
    ///
    /// In an app, autumn calls this at shutdown. autumn gives all shutdown
    /// hooks `[server] shutdown_timeout_secs` in total, so the wait can be
    /// shorter than `drain_timeout_secs`.
    pub async fn shutdown(&self) {
        drain(&self.engine).await;
    }
}

/// Closes the tracker and waits for its tasks, up to the drain timeout.
async fn drain(engine: &Engine) {
    let tracker = &engine.runner.tracker;
    tracker.close();
    let limit = Duration::from_secs(engine.config.drain_timeout_secs);
    if tokio::time::timeout(limit, tracker.wait()).await.is_err() {
        tracing::warn!(
            left = tracker.len(),
            "teams: handlers still run after the drain timeout"
        );
    }
}

impl Plugin for TeamsPlugin {
    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(PLUGIN_NAME)
    }

    fn contract(&self) -> Option<PluginContract> {
        Some(
            PluginContract::new(env!("CARGO_PKG_NAME"))
                .plugin_version(env!("CARGO_PKG_VERSION"))
                .autumn_web("0.8"),
        )
    }

    fn build(self, app: AppBuilder) -> AppBuilder {
        let base = self.base_path.clone();
        let cell: EngineCell = Arc::new(OnceLock::new());
        let health = Arc::new(TeamsHealth::new(self.readiness));
        let metrics = Arc::clone(&self.metrics);
        let failure: Arc<Mutex<Option<TeamsError>>> = Arc::default();
        let routes = routes::autumn_routes(&cell, &base);
        let (init_cell, init_health, init_failure) =
            (Arc::clone(&cell), Arc::clone(&health), Arc::clone(&failure));
        let (check_cell, stop_cell) = (Arc::clone(&cell), Arc::clone(&cell));
        app.config_section(SECTION)
            .metrics_source("teams", metrics as Arc<dyn MetricsSource>)
            .health_indicator("teams", health as Arc<dyn HealthIndicator>)
            .routes(routes)
            // Start before job workers, so a `#[job]` can use `TeamsClient::from_state`.
            .state_initializer(move |state| {
                match self.start_inner(state, state.role(), init_health) {
                    Ok(rt) => {
                        let _ = init_cell.set(rt.engine);
                    }
                    Err(e) => {
                        // Also log: some autumn modes run no startup hook.
                        tracing::error!(error = %e, "teams plugin did not start");
                        *init_failure
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(e);
                    }
                }
            })
            // An initializer cannot fail the boot. This hook reports its error.
            .on_startup(move |_state| {
                let failed = failure
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                let started = check_cell.get().is_some();
                async move {
                    match failed {
                        Some(e) => Err(AutumnError::internal_server_error(e)),
                        None if !started => Err(AutumnError::internal_server_error(
                            TeamsError::Config("the teams plugin did not start".to_owned()),
                        )),
                        None => Ok(()),
                    }
                }
            })
            .on_shutdown(move || shutdown_hook(&stop_cell))
    }
}

/// The autumn shutdown hook: drain the started engine, if any.
fn shutdown_hook(cell: &EngineCell) -> impl Future<Output = ()> + Send + use<> {
    let engine = cell.get().cloned();
    async move {
        if let Some(engine) = engine {
            drain(&engine).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_path_normalizes() {
        assert_eq!(normalize_base("/teams/"), "/teams");
        assert_eq!(normalize_base("hooks/teams"), "/hooks/teams");
        assert_eq!(normalize_base("/"), "");
        assert_eq!(route_path(""), "/messages");
    }

    #[test]
    fn exempt_rule_matches_autumn() {
        let ex = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert!(is_exempt("/teams/messages", &ex(&["/teams"])));
        assert!(is_exempt("/teams/messages", &ex(&["/teams/"])));
        assert!(is_exempt("/teams/messages", &ex(&["/teams/messages"])));
        assert!(!is_exempt("/teams/messages", &ex(&["/tea"])));
        assert!(!is_exempt("/teams/messages", &ex(&["/teams/m"])));
        assert!(is_exempt("/teams/messages", &ex(&[""])));
    }

    #[test]
    fn csrf_check_names_the_fix() {
        let mut cfg = AutumnConfig::default();
        check_security(&cfg, "/teams").unwrap();
        cfg.security.csrf.enabled = true;
        let e = check_security(&cfg, "/teams").unwrap_err().to_string();
        assert!(e.contains("[\"/teams\"]"), "{e}");
        // At the root, the fix names the exact path, not "/".
        let e = check_security(&cfg, "").unwrap_err().to_string();
        assert!(e.contains("[\"/messages\"]"), "{e}");
        cfg.security.csrf.exempt_paths = vec!["/teams".into()];
        check_security(&cfg, "/teams").unwrap();
        // A signed-webhook endpoint path is also exempt in autumn.
        let mut cfg = AutumnConfig::default();
        cfg.security.csrf.enabled = true;
        cfg.security
            .webhooks
            .endpoints
            .push(autumn_web::webhook::WebhookEndpointConfig::slack(
                "s",
                "/teams/messages",
                "S",
            ));
        check_security(&cfg, "/teams").unwrap();
    }
}
