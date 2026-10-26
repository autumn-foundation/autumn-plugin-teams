//! Microsoft Teams plugin for autumn-web.
//!
//! - **Inbound:** a Bot Framework route that checks the Connector JWT.
//!   Handlers by command, activity type, event name, invoke name, or
//!   Adaptive Card verb.
//! - **Outbound:** a Connector REST client with an app token, retry rules,
//!   and a `serviceUrl` host allowlist. Proactive messages.
//! - **Operations:** health indicator, Prometheus metrics, drain on shutdown.
//!
//! See the README for setup.

pub mod activity;
pub mod auth;
mod client;
pub mod config;
mod engine;
mod error;
mod handlers;
mod health;
mod metrics;
mod plugin;
pub mod policy;
mod routes;
#[cfg(feature = "testing")]
pub mod testing;
mod token;
pub mod transport;

pub use client::TeamsClient;
pub use config::TeamsConfig;
pub use error::{HandlerError, HandlerResult, TeamsError};
pub use handlers::TeamsContext;
pub use health::TeamsHealth;
pub use metrics::TeamsMetrics;
pub use plugin::{DEFAULT_BASE_PATH, PLUGIN_NAME, TeamsPlugin, TeamsRuntime};
