//! Bot Connector REST client.

use std::fmt::Write as _;
use std::sync::Arc;

use autumn_web::AppState;
use serde_json::Value;

use serde::de::DeserializeOwned;

use crate::activity::{
    Activity, ChannelInfo, ConversationAccount, ConversationParameters, ConversationReference,
    ConversationResourceResponse, PagedMembers, ResourceResponse, TeamDetails, TeamsChannelAccount,
};
use crate::config::{TeamsConfig, is_loopback};
use crate::error::TeamsError;
use crate::metrics::TeamsMetrics;
use crate::policy::{Step, decide, retry_after_ms};
use crate::token::{TokenProvider, classify};
use crate::transport::{HttpReply, HttpRequest, HttpTransport, Method, TransportError};

struct Inner {
    transport: Arc<dyn HttpTransport>,
    config: TeamsConfig,
    tokens: Arc<TokenProvider>,
    metrics: Arc<TeamsMetrics>,
}

/// Bot Connector REST client. A clone shares one connection pool and one
/// token cache.
///
/// Get it from a handler with [`crate::TeamsContext::client`], or from the
/// app state with [`TeamsClient::from_state`].
///
/// The client sends the app token only to a `serviceUrl` whose host is in
/// `[teams] service_url_hosts`.
#[derive(Clone)]
pub struct TeamsClient {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for TeamsClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeamsClient")
            .field("service_url_hosts", &self.inner.config.service_url_hosts)
            .field("has_credentials", &self.has_credentials())
            .finish_non_exhaustive()
    }
}

impl TeamsClient {
    pub(crate) fn new(
        transport: Arc<dyn HttpTransport>,
        config: TeamsConfig,
        tokens: Arc<TokenProvider>,
        metrics: Arc<TeamsMetrics>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                transport,
                config,
                tokens,
                metrics,
            }),
        }
    }

    /// Returns the client that the plugin put in the app state at startup.
    #[must_use]
    pub fn from_state(state: &AppState) -> Option<Self> {
        state.extension::<Self>().map(|c| (*c).clone())
    }

    /// Returns `true` when an app password is set.
    #[must_use]
    pub fn has_credentials(&self) -> bool {
        self.inner.tokens.has_password()
    }

    /// Gets an app token. Uses the cache. The health check uses it.
    ///
    /// # Errors
    /// Returns [`TeamsError::Token`] or a config error.
    pub async fn check_credentials(&self) -> Result<(), TeamsError> {
        self.inner.tokens.token().await.map(drop)
    }

    /// The full URL: `service_url`, then the encoded `segments`, then the
    /// query.
    fn url(
        &self,
        service_url: &str,
        segments: &[&str],
        query: &[(&str, &str)],
    ) -> Result<String, TeamsError> {
        if !allowed_service_url(service_url, &self.inner.config.service_url_hosts) {
            return Err(TeamsError::BadServiceUrl);
        }
        let mut url = service_url.trim_end_matches('/').to_owned();
        for seg in segments {
            url.push('/');
            url.push_str(&encode_segment(seg));
        }
        if !query.is_empty() {
            url.push('?');
            url.push_str(
                &serde_urlencoded::to_string(query)
                    .map_err(|e| TeamsError::Config(format!("query: {e}")))?,
            );
        }
        Ok(url)
    }

    /// Calls a Connector operation. `segments` are path parts after the
    /// `serviceUrl`. The client encodes each part. `operation` is the metrics
    /// label. GET, PUT, and DELETE also retry on 5xx and network errors.
    ///
    /// # Errors
    /// Returns [`TeamsError::Api`] for an error status, or a transport,
    /// rate-limit, token, decode, `serviceUrl`, or config error.
    pub async fn call(
        &self,
        operation: &'static str,
        method: Method,
        service_url: &str,
        segments: &[&str],
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<Value, TeamsError> {
        let url = self.url(service_url, segments, query)?;
        let body = body
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|e| TeamsError::Config(format!("{operation}: {e}")))?;
        let (inner, rule) = (&self.inner, self.inner.config.api.rule());
        let mut attempt: u32 = 1;
        let mut refreshed = false;
        loop {
            let token = inner.tokens.token().await.inspect_err(|_| {
                inner.metrics.api_call(operation, "token_error");
            })?;
            let mut req = HttpRequest::new(method, url.as_str())
                .header("authorization", format!("Bearer {token}"));
            if let Some(b) = &body {
                req = req
                    .header("content-type", "application/json; charset=utf-8")
                    .body(b.clone());
            }
            let result = inner.transport.send(req).await;
            let (outcome, retry_after) = classify(&result);
            match decide(
                outcome,
                attempt,
                method.is_idempotent(),
                retry_after.map(retry_after_ms),
                refreshed,
                &rule,
            ) {
                Step::Retry(wait_ms) => {
                    inner.metrics.api_retry(operation);
                    tracing::debug!(operation, attempt, wait_ms, "teams api retry");
                    tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
                }
                Step::RefreshToken => {
                    // The Connector did nothing. Get a new token and try again.
                    inner.metrics.api_retry(operation);
                    inner.tokens.invalidate(&token);
                    refreshed = true;
                }
                Step::Done | Step::GiveUp => return self.finish(operation, result),
            }
            attempt += 1;
        }
    }

    /// Turns the last attempt into a result. Counts it.
    fn finish(
        &self,
        operation: &'static str,
        result: Result<HttpReply, TransportError>,
    ) -> Result<Value, TeamsError> {
        let m = &self.inner.metrics;
        let reply = result.map_err(|e| {
            m.api_call(operation, "transport");
            TeamsError::Transport(e.0)
        })?;
        if reply.status == 429 {
            m.api_call(operation, "rate_limited");
            return Err(TeamsError::RateLimited {
                retry_after_secs: reply.retry_after_secs,
            });
        }
        if !(200..300).contains(&reply.status) {
            m.api_call(operation, "http_error");
            let err = serde_json::from_slice::<Value>(&reply.body)
                .ok()
                .and_then(|v| v.get("error").cloned());
            let text = |k: &str| {
                err.as_ref()
                    .and_then(|e| e.get(k))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            };
            return Err(TeamsError::Api {
                operation,
                status: reply.status,
                code: text("code").unwrap_or_else(|| "unknown".to_owned()),
                message: text("message"),
            });
        }
        if reply.body.iter().all(u8::is_ascii_whitespace) {
            m.api_call(operation, "ok");
            return Ok(Value::Null);
        }
        let v = serde_json::from_slice(&reply.body).map_err(|e| {
            m.api_call(operation, "decode");
            TeamsError::Decode(format!("{operation}: {e}"))
        })?;
        m.api_call(operation, "ok");
        Ok(v)
    }

    /// Calls and decodes the reply as `T`. An empty reply gives the default.
    async fn call_as<T: DeserializeOwned + Default>(
        &self,
        operation: &'static str,
        method: Method,
        service_url: &str,
        segments: &[&str],
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<T, TeamsError> {
        let v = self
            .call(operation, method, service_url, segments, query, body)
            .await?;
        if v.is_null() {
            return Ok(T::default());
        }
        T::deserialize(&v).map_err(|e| TeamsError::Decode(format!("{operation}: {e}")))
    }

    /// `POST /v3/conversations/{id}/activities`.
    ///
    /// # Errors
    /// Same as [`Self::call`].
    pub async fn send_to_conversation(
        &self,
        service_url: &str,
        conversation_id: &str,
        activity: &Activity,
    ) -> Result<ResourceResponse, TeamsError> {
        let body = outgoing(activity, conversation_id, None)?;
        self.call_as(
            "send_to_conversation",
            Method::Post,
            service_url,
            &["v3", "conversations", conversation_id, "activities"],
            &[],
            Some(&body),
        )
        .await
    }

    /// `POST /v3/conversations/{id}/activities/{activityId}`: a reply.
    ///
    /// # Errors
    /// Same as [`Self::call`].
    pub async fn reply_to_activity(
        &self,
        service_url: &str,
        conversation_id: &str,
        activity_id: &str,
        activity: &Activity,
    ) -> Result<ResourceResponse, TeamsError> {
        let mut a = activity.clone();
        a.reply_to_id = Some(activity_id.to_owned());
        let body = outgoing(&a, conversation_id, None)?;
        self.call_as(
            "reply_to_activity",
            Method::Post,
            service_url,
            &[
                "v3",
                "conversations",
                conversation_id,
                "activities",
                activity_id,
            ],
            &[],
            Some(&body),
        )
        .await
    }

    /// `PUT /v3/conversations/{id}/activities/{activityId}`.
    ///
    /// # Errors
    /// Same as [`Self::call`].
    pub async fn update_activity(
        &self,
        service_url: &str,
        conversation_id: &str,
        activity_id: &str,
        activity: &Activity,
    ) -> Result<ResourceResponse, TeamsError> {
        let body = outgoing(activity, conversation_id, Some(activity_id))?;
        self.call_as(
            "update_activity",
            Method::Put,
            service_url,
            &[
                "v3",
                "conversations",
                conversation_id,
                "activities",
                activity_id,
            ],
            &[],
            Some(&body),
        )
        .await
    }

    /// `DELETE /v3/conversations/{id}/activities/{activityId}`.
    ///
    /// # Errors
    /// Same as [`Self::call`].
    pub async fn delete_activity(
        &self,
        service_url: &str,
        conversation_id: &str,
        activity_id: &str,
    ) -> Result<(), TeamsError> {
        self.call(
            "delete_activity",
            Method::Delete,
            service_url,
            &[
                "v3",
                "conversations",
                conversation_id,
                "activities",
                activity_id,
            ],
            &[],
            None,
        )
        .await
        .map(drop)
    }

    /// `POST /v3/conversations`: starts a conversation.
    ///
    /// # Errors
    /// Same as [`Self::call`].
    pub async fn create_conversation(
        &self,
        service_url: &str,
        params: &ConversationParameters,
    ) -> Result<ConversationResourceResponse, TeamsError> {
        let body = serde_json::to_value(params)
            .map_err(|e| TeamsError::Config(format!("create_conversation: {e}")))?;
        self.call_as(
            "create_conversation",
            Method::Post,
            service_url,
            &["v3", "conversations"],
            &[],
            Some(&body),
        )
        .await
    }

    /// `GET /v3/conversations/{id}/members`.
    ///
    /// # Errors
    /// Same as [`Self::call`].
    pub async fn get_members(
        &self,
        service_url: &str,
        conversation_id: &str,
    ) -> Result<Vec<TeamsChannelAccount>, TeamsError> {
        self.call_as(
            "get_members",
            Method::Get,
            service_url,
            &["v3", "conversations", conversation_id, "members"],
            &[],
            None,
        )
        .await
    }

    /// `GET /v3/conversations/{id}/pagedmembers`: one page.
    ///
    /// # Errors
    /// Same as [`Self::call`].
    pub async fn get_paged_members(
        &self,
        service_url: &str,
        conversation_id: &str,
        page_size: Option<u32>,
        continuation_token: Option<&str>,
    ) -> Result<PagedMembers, TeamsError> {
        let size = page_size.map(|n| n.to_string());
        let mut query = Vec::new();
        if let Some(n) = &size {
            query.push(("pageSize", n.as_str()));
        }
        if let Some(t) = continuation_token {
            query.push(("continuationToken", t));
        }
        self.call_as(
            "get_paged_members",
            Method::Get,
            service_url,
            &["v3", "conversations", conversation_id, "pagedmembers"],
            &query,
            None,
        )
        .await
    }

    /// All members: follows `continuationToken` for up to `max_pages` pages
    /// of 500.
    ///
    /// # Errors
    /// Same as [`Self::call`].
    pub async fn all_members(
        &self,
        service_url: &str,
        conversation_id: &str,
        max_pages: usize,
    ) -> Result<Vec<TeamsChannelAccount>, TeamsError> {
        let mut all = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..max_pages {
            let page = self
                .get_paged_members(service_url, conversation_id, Some(500), token.as_deref())
                .await?;
            all.extend(page.members);
            token = page.continuation_token.filter(|t| !t.is_empty());
            if token.is_none() {
                break;
            }
        }
        Ok(all)
    }

    /// `GET /v3/conversations/{id}/members/{memberId}`.
    ///
    /// # Errors
    /// Same as [`Self::call`].
    pub async fn get_member(
        &self,
        service_url: &str,
        conversation_id: &str,
        member_id: &str,
    ) -> Result<TeamsChannelAccount, TeamsError> {
        self.call_as(
            "get_member",
            Method::Get,
            service_url,
            &["v3", "conversations", conversation_id, "members", member_id],
            &[],
            None,
        )
        .await
    }

    /// `GET /v3/teams/{teamId}`.
    ///
    /// # Errors
    /// Same as [`Self::call`].
    pub async fn team_details(
        &self,
        service_url: &str,
        team_id: &str,
    ) -> Result<TeamDetails, TeamsError> {
        self.call_as(
            "team_details",
            Method::Get,
            service_url,
            &["v3", "teams", team_id],
            &[],
            None,
        )
        .await
    }

    /// `GET /v3/teams/{teamId}/conversations`: the channels of a team.
    ///
    /// # Errors
    /// Same as [`Self::call`].
    pub async fn team_channels(
        &self,
        service_url: &str,
        team_id: &str,
    ) -> Result<Vec<ChannelInfo>, TeamsError> {
        #[derive(Default, serde::Deserialize)]
        struct Reply {
            #[serde(default)]
            conversations: Vec<ChannelInfo>,
        }
        let r: Reply = self
            .call_as(
                "team_channels",
                Method::Get,
                service_url,
                &["v3", "teams", team_id, "conversations"],
                &[],
                None,
            )
            .await?;
        Ok(r.conversations)
    }

    /// Sends `activity` to a stored conversation: a proactive message. It
    /// sets `from`, `recipient`, `conversation`, and `channelId` from the
    /// reference when they are empty.
    ///
    /// # Errors
    /// Same as [`Self::call`].
    pub async fn send_to(
        &self,
        reference: &ConversationReference,
        activity: &Activity,
    ) -> Result<ResourceResponse, TeamsError> {
        let a = addressed(activity, reference);
        self.send_to_conversation(&reference.service_url, &reference.conversation.id, &a)
            .await
    }
}

/// Fills `from`, `recipient`, `conversation`, and `channelId` from the
/// reference when they are empty.
pub(crate) fn addressed(activity: &Activity, reference: &ConversationReference) -> Activity {
    let mut a = activity.clone();
    if a.from.is_none() {
        a.from.clone_from(&reference.bot);
    }
    if a.recipient.is_none() {
        a.recipient.clone_from(&reference.user);
    }
    if a.conversation.is_none() {
        a.conversation = Some(reference.conversation.clone());
    }
    if a.channel_id.is_none() && !reference.channel_id.is_empty() {
        a.channel_id = Some(reference.channel_id.clone());
    }
    a
}

/// The JSON body of an outgoing activity: the conversation is set, and `id`
/// for an update.
fn outgoing(
    activity: &Activity,
    conversation_id: &str,
    id: Option<&str>,
) -> Result<Value, TeamsError> {
    let mut a = activity.clone();
    if a.conversation.is_none() {
        a.conversation = Some(ConversationAccount::new(conversation_id));
    }
    if let Some(id) = id {
        a.id = Some(id.to_owned());
    }
    serde_json::to_value(&a).map_err(|e| TeamsError::Config(format!("activity: {e}")))
}

/// Percent-encodes one path segment. Only unreserved characters stay, like
/// `encodeURIComponent` in the Bot Framework SDK.
fn encode_segment(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    for b in seg.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Returns `true` when the bot token may go to `url`: HTTPS (or HTTP on a
/// loopback host), a host in `hosts`, no user info, and no port (except on
/// loopback).
pub(crate) fn allowed_service_url(url: &str, hosts: &[String]) -> bool {
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    if !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
    {
        return false;
    }
    let host = match u.host() {
        Some(url::Host::Domain(d)) => d.to_owned(),
        Some(url::Host::Ipv4(ip)) => ip.to_string(),
        Some(url::Host::Ipv6(ip)) => ip.to_string(),
        None => return false,
    };
    if !hosts.iter().any(|h| h.eq_ignore_ascii_case(&host)) {
        return false;
    }
    // `url` drops the default port, so `:443` on https is `None`.
    match u.scheme() {
        "https" => u.port().is_none(),
        "http" => is_loopback(u.host().as_ref()),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn hosts(list: &[&str]) -> Vec<String> {
        list.iter().map(|h| (*h).to_owned()).collect()
    }

    #[test]
    fn service_url_rules() {
        let h = hosts(&["smba.trafficmanager.net", "127.0.0.1"]);
        for ok in [
            "https://smba.trafficmanager.net/teams/",
            "https://smba.trafficmanager.net/amer",
            "https://SMBA.TrafficManager.net/",
            "https://smba.trafficmanager.net:443/teams/",
            "http://127.0.0.1:8080/teams/",
        ] {
            assert!(allowed_service_url(ok, &h), "{ok}");
        }
        for bad in [
            "http://smba.trafficmanager.net/",
            "https://smba.trafficmanager.net:444/",
            "https://a@smba.trafficmanager.net/",
            "https://smba.trafficmanager.net.evil.example/",
            "https://evil.smba.trafficmanager.net/",
            "https://smba.trafficmanager.net./",
            "https://127.0.0.2/",
            "http://localhost/",
            "https://smba.trafficmanager.net/teams/?x=1",
            "https://smba.trafficmanager.net/teams/#f",
            "",
        ] {
            assert!(!allowed_service_url(bad, &h), "{bad}");
        }
    }

    proptest! {
        /// Any host that is not in the list fails, whatever its shape.
        #[test]
        fn only_listed_hosts_pass(host in "[a-z0-9.-]{1,30}") {
            let h = hosts(&["smba.trafficmanager.net"]);
            let url = format!("https://{host}/teams/");
            if allowed_service_url(&url, &h) {
                prop_assert_eq!(host.trim_end_matches('.'), "smba.trafficmanager.net");
                prop_assert!(!host.ends_with('.'));
            }
        }
    }
}
