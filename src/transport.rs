//! HTTP transport. Only this module does network I/O.
//!
//! - [`ReqwestTransport`]: real HTTPS.
//! - [`MemoryTransport`]: a fake for tests. It records requests and replies
//!   with set responses.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;

/// HTTP method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    /// `GET`.
    Get,
    /// `POST`.
    Post,
    /// `PUT`.
    Put,
    /// `DELETE`.
    Delete,
}

impl Method {
    /// The method name, for example `GET`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
        }
    }

    /// Returns `true` when a repeat has the same effect as one call.
    #[must_use]
    pub const fn is_idempotent(self) -> bool {
        !matches!(self, Self::Post)
    }
}

/// One HTTP request.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequest {
    /// HTTP method.
    pub method: Method,
    /// Full URL.
    pub url: String,
    /// Header names in lower case, and values.
    pub headers: Vec<(String, String)>,
    /// Body bytes.
    pub body: Vec<u8>,
}

impl std::fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &redacted(&self.headers))
            .field("body_len", &self.body.len())
            .finish()
    }
}

/// Header pairs with the `authorization` value hidden.
fn redacted(headers: &[(String, String)]) -> Vec<(&str, &str)> {
    headers
        .iter()
        .map(|(k, v)| {
            let v = if k == "authorization" {
                "<redacted>"
            } else {
                v.as_str()
            };
            (k.as_str(), v)
        })
        .collect()
}

impl HttpRequest {
    /// Makes a request with no headers and no body.
    #[must_use]
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// Adds a header. The name becomes lower case.
    #[must_use]
    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_ascii_lowercase(), value.into()));
        self
    }

    /// Sets the body.
    #[must_use]
    pub fn body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }
}

/// One HTTP reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpReply {
    /// HTTP status.
    pub status: u16,
    /// The `Retry-After` header, in seconds, if any.
    pub retry_after_secs: Option<u64>,
    /// Body bytes.
    pub body: Vec<u8>,
}

impl HttpReply {
    /// A reply with a JSON body.
    #[must_use]
    pub fn json(status: u16, body: &serde_json::Value) -> Self {
        Self {
            status,
            retry_after_secs: None,
            body: body.to_string().into_bytes(),
        }
    }

    /// A reply with a text body.
    #[must_use]
    pub fn text(status: u16, body: &str) -> Self {
        Self {
            status,
            retry_after_secs: None,
            body: body.as_bytes().to_vec(),
        }
    }

    /// Sets `Retry-After`.
    #[must_use]
    pub const fn retry_after(mut self, secs: u64) -> Self {
        self.retry_after_secs = Some(secs);
        self
    }
}

/// No HTTP reply: connect error or timeout. The text has no header values.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct TransportError(pub String);

/// Sends HTTP requests.
pub trait HttpTransport: Send + Sync + 'static {
    /// Sends `req` and returns the reply.
    fn send(&self, req: HttpRequest) -> BoxFuture<'_, Result<HttpReply, TransportError>>;
}

/// Real HTTPS transport (`reqwest`, rustls).
#[derive(Debug, Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    /// Makes a transport with this time limit per request.
    ///
    /// # Errors
    /// Returns [`TransportError`] when the TLS setup fails.
    pub fn new(timeout: Duration) -> Result<Self, TransportError> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            // A redirect could send the bot token to another host.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("autumn-plugin-teams/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| TransportError(format!("client setup: {e}")))?;
        Ok(Self { client })
    }
}

impl HttpTransport for ReqwestTransport {
    fn send(&self, req: HttpRequest) -> BoxFuture<'_, Result<HttpReply, TransportError>> {
        Box::pin(async move {
            let method = match req.method {
                Method::Get => reqwest::Method::GET,
                Method::Post => reqwest::Method::POST,
                Method::Put => reqwest::Method::PUT,
                Method::Delete => reqwest::Method::DELETE,
            };
            let mut builder = self.client.request(method, &req.url).body(req.body);
            for (k, v) in &req.headers {
                builder = builder.header(k, v);
            }
            // `without_url`: the error text has no URL query, and no header.
            let res = builder
                .send()
                .await
                .map_err(|e| TransportError(e.without_url().to_string()))?;
            let status = res.status().as_u16();
            let retry_after_secs = res
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse().ok());
            let body = res
                .bytes()
                .await
                .map_err(|e| TransportError(e.without_url().to_string()))?
                .to_vec();
            Ok(HttpReply {
                status,
                retry_after_secs,
                body,
            })
        })
    }
}

/// A request that [`MemoryTransport`] got.
#[derive(Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    /// HTTP method.
    pub method: Method,
    /// Full URL.
    pub url: String,
    /// Header names in lower case, and values.
    pub headers: Vec<(String, String)>,
    /// Body bytes.
    pub body: Vec<u8>,
}

impl std::fmt::Debug for RecordedRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordedRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &redacted(&self.headers))
            .field("body_len", &self.body.len())
            .finish()
    }
}

impl RecordedRequest {
    /// Returns the first header with this name.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<String> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    }

    /// Decodes a form body. Returns an empty map for a bad body.
    #[must_use]
    pub fn form(&self) -> std::collections::BTreeMap<String, String> {
        serde_urlencoded::from_bytes(&self.body).unwrap_or_default()
    }

    /// Decodes a JSON body. Returns `Null` for a bad body.
    #[must_use]
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }
}

#[derive(Default)]
struct MemoryInner {
    requests: Vec<RecordedRequest>,
    /// Replies for each pattern. The last reply repeats.
    replies: HashMap<String, VecDeque<HttpReply>>,
    /// Number of network errors to give for each pattern before a reply.
    failures: HashMap<String, u32>,
    /// Wait before each reply.
    latency: Duration,
}

/// The access token that [`MemoryTransport`] gives by default.
pub const MEMORY_ACCESS_TOKEN: &str = "memory-access-token";

/// In-memory fake transport for tests.
///
/// A pattern is `"<text>"` or `"<METHOD> <text>"`. It matches a request
/// whose URL contains the text (and that has the method). The longest
/// matching pattern wins. With no set reply:
///
/// - a URL that contains `/oauth2/v2.0/token` gets a valid token reply,
/// - other URLs get `200 {"id": "1"}`.
///
/// Clones share state.
#[derive(Clone, Default)]
pub struct MemoryTransport {
    inner: Arc<Mutex<MemoryInner>>,
}

impl std::fmt::Debug for MemoryTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryTransport").finish_non_exhaustive()
    }
}

impl MemoryTransport {
    /// Makes an empty fake.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replies `reply` to each request that matches `pattern`.
    pub fn respond(&self, pattern: &str, reply: HttpReply) {
        self.respond_seq(pattern, vec![reply]);
    }

    /// Replies with `replies` in order. The last one repeats.
    pub fn respond_seq(&self, pattern: &str, replies: Vec<HttpReply>) {
        self.lock()
            .replies
            .insert(pattern.to_owned(), replies.into());
    }

    /// Waits `latency` before each reply. Default: no wait.
    pub fn set_latency(&self, latency: Duration) {
        self.lock().latency = latency;
    }

    /// Gives a network error to the next `count` requests that match.
    pub fn fail_next(&self, pattern: &str, count: u32) {
        self.lock().failures.insert(pattern.to_owned(), count);
    }

    /// Returns all requests.
    #[must_use]
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.lock().requests.clone()
    }

    /// Returns the requests that match `pattern`.
    #[must_use]
    pub fn requests_to(&self, pattern: &str) -> Vec<RecordedRequest> {
        self.lock()
            .requests
            .iter()
            .filter(|r| matches(r.method, &r.url, pattern))
            .cloned()
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MemoryInner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn reply_for(&self, method: Method, url: &str) -> Result<HttpReply, TransportError> {
        let mut inner = self.lock();
        if let Some(n) = inner
            .failures
            .iter_mut()
            .filter(|(p, n)| **n > 0 && matches(method, url, p))
            .max_by_key(|(p, _)| p.len())
            .map(|(_, n)| n)
        {
            *n -= 1;
            drop(inner);
            return Err(TransportError("memory transport: set failure".to_owned()));
        }
        let reply = inner
            .replies
            .iter_mut()
            .filter(|(p, _)| matches(method, url, p))
            .max_by_key(|(p, _)| p.len())
            .and_then(|(_, q)| {
                if q.len() > 1 {
                    q.pop_front()
                } else {
                    q.front().cloned()
                }
            });
        drop(inner);
        Ok(reply.unwrap_or_else(|| default_reply(url)))
    }
}

fn default_reply(url: &str) -> HttpReply {
    if url.contains("/oauth2/v2.0/token") {
        HttpReply::json(
            200,
            &serde_json::json!({
                "token_type": "Bearer",
                "expires_in": 3600,
                "access_token": MEMORY_ACCESS_TOKEN,
            }),
        )
    } else {
        HttpReply::json(200, &serde_json::json!({"id": "1"}))
    }
}

/// `"<text>"` matches a URL that contains the text. `"<METHOD> <text>"` also
/// needs the method.
fn matches(method: Method, url: &str, pattern: &str) -> bool {
    let (want, text) = match pattern.split_once(' ') {
        Some((m, rest)) if m.chars().all(|c| c.is_ascii_uppercase()) => (Some(m), rest),
        _ => (None, pattern),
    };
    want.is_none_or(|m| m == method.as_str()) && url.contains(text)
}

impl HttpTransport for MemoryTransport {
    fn send(&self, req: HttpRequest) -> BoxFuture<'_, Result<HttpReply, TransportError>> {
        Box::pin(async move {
            let latency = self.lock().latency;
            if !latency.is_zero() {
                tokio::time::sleep(latency).await;
            }
            let reply = self.reply_for(req.method, &req.url);
            self.lock().requests.push(RecordedRequest {
                method: req.method,
                url: req.url,
                headers: req.headers,
                body: req.body,
            });
            reply
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memory_default_seq_and_failures() {
        let t = MemoryTransport::new();
        let url = "https://smba.trafficmanager.net/teams/v3/conversations/c1/activities";
        let r = t.send(HttpRequest::new(Method::Post, url)).await.unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, br#"{"id":"1"}"#);
        let tok = t
            .send(HttpRequest::new(
                Method::Post,
                "https://login.microsoftonline.com/x/oauth2/v2.0/token",
            ))
            .await
            .unwrap();
        assert!(
            String::from_utf8(tok.body)
                .unwrap()
                .contains(MEMORY_ACCESS_TOKEN)
        );
        t.respond_seq(
            "/activities",
            vec![HttpReply::text(500, "a"), HttpReply::text(201, "b")],
        );
        // The longer pattern wins. A method prefix must match.
        t.respond(
            "POST /conversations/c1/activities",
            HttpReply::text(202, "c"),
        );
        t.respond(
            "GET /conversations/c1/activities",
            HttpReply::text(203, "d"),
        );
        let send = |m| t.send(HttpRequest::new(m, url));
        assert_eq!(send(Method::Post).await.unwrap().status, 202);
        assert_eq!(send(Method::Get).await.unwrap().status, 203);
        assert_eq!(send(Method::Put).await.unwrap().status, 500);
        assert_eq!(send(Method::Put).await.unwrap().status, 201);
        assert_eq!(send(Method::Put).await.unwrap().status, 201);
        t.fail_next("/activities", 1);
        assert!(send(Method::Delete).await.is_err());
        assert_eq!(send(Method::Delete).await.unwrap().status, 201);
        assert_eq!(t.requests_to("PUT /activities").len(), 3);
        assert_eq!(t.requests_to("/activities").len(), 8);
        assert_eq!(t.requests().len(), 9);
        assert!(t.requests_to("/members").is_empty());
    }

    #[test]
    fn recorded_decoders_and_debug_redaction() {
        let r = RecordedRequest {
            method: Method::Post,
            url: "u".into(),
            headers: vec![
                ("x-a".into(), "1".into()),
                ("authorization".into(), "Bearer secret-1".into()),
            ],
            body: b"a=1&b=two+words".to_vec(),
        };
        assert_eq!(r.header("X-A").as_deref(), Some("1"));
        assert_eq!(r.form()["b"], "two words");
        assert!(r.json().is_null());
        assert!(!format!("{r:?}").contains("secret-1"));
        let q = HttpRequest::new(Method::Get, "u").header("Authorization", "Bearer secret-2");
        assert!(!format!("{q:?}").contains("secret-2"));
        assert!(format!("{:?}", MemoryTransport::new()).contains("MemoryTransport"));
        assert!(Method::Get.is_idempotent() && Method::Put.is_idempotent());
        assert!(Method::Delete.is_idempotent() && !Method::Post.is_idempotent());
    }
}
