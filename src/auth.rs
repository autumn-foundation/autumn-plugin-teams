//! Inbound auth: the Bot Framework JWT check.
//!
//! The check runs in two parts:
//!
//! 1. [`Verifier::verify`] checks the `Authorization` header before the body
//!    is parsed: `Bearer` scheme, RS256, a known `kid`, the signature, the
//!    issuer, the audience, and the time window.
//! 2. [`check_activity`] checks the parsed activity against the token: the
//!    key endorsement for `channelId`, the `serviceUrl` claim, and the
//!    tenant and channel allowlists.

use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use serde_json::Value;

use crate::activity::Activity;
use crate::error::TeamsError;
use crate::metrics::TeamsMetrics;
use crate::policy::{keys_fresh, may_refresh_keys, token_time_valid};
use crate::transport::{HttpRequest, HttpTransport, Method};

/// Largest `Authorization` header that the check reads.
pub const MAX_TOKEN_BYTES: usize = 16 * 1024;

/// Why a request failed the check.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AuthError {
    /// No `Authorization` header.
    #[error("no Authorization header")]
    MissingToken,
    /// Not a `Bearer` token, or not a JWT.
    #[error("malformed bearer token")]
    Malformed,
    /// The token algorithm is not RS256.
    #[error("token algorithm is not RS256")]
    Algorithm,
    /// No key has the token `kid`.
    #[error("unknown signing key")]
    UnknownKey,
    /// The signature does not verify.
    #[error("bad token signature")]
    Signature,
    /// The issuer is not accepted.
    #[error("token issuer is not accepted")]
    Issuer,
    /// The audience is not the app ID.
    #[error("token audience is not the app ID")]
    Audience,
    /// The token is expired or not yet valid.
    #[error("token is expired or not yet valid")]
    Expired,
    /// The key set cannot be loaded.
    #[error("signing keys are not available")]
    KeysUnavailable,
    /// The signing key has no endorsement for the activity channel.
    #[error("signing key has no endorsement for the channel")]
    Endorsement,
    /// The `serviceUrl` claim does not match the activity.
    #[error("serviceUrl claim does not match the activity")]
    ServiceUrl,
    /// The channel is not in `allowed_channels`.
    #[error("channel is not allowed")]
    Channel,
    /// The tenant is not in `allowed_tenants`.
    #[error("tenant is not allowed")]
    Tenant,
}

impl AuthError {
    /// HTTP status for this error: 401, 403, or 503.
    #[must_use]
    pub const fn status(&self) -> u16 {
        match self {
            Self::KeysUnavailable => 503,
            Self::Endorsement | Self::ServiceUrl | Self::Channel | Self::Tenant => 403,
            _ => 401,
        }
    }

    /// Metrics label.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::MissingToken => "missing_token",
            Self::Malformed => "malformed_token",
            Self::Algorithm => "bad_algorithm",
            Self::UnknownKey => "unknown_key",
            Self::Signature => "bad_signature",
            Self::Issuer => "bad_issuer",
            Self::Audience => "bad_audience",
            Self::Expired => "expired",
            Self::KeysUnavailable => "keys_unavailable",
            Self::Endorsement => "no_endorsement",
            Self::ServiceUrl => "service_url_mismatch",
            Self::Channel => "channel_not_allowed",
            Self::Tenant => "tenant_not_allowed",
        }
    }
}

/// One RSA signing key from a JWKS document.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct SigningKey {
    /// Key ID.
    pub kid: String,
    /// RSA modulus, base64url.
    pub n: String,
    /// RSA exponent, base64url.
    pub e: String,
    /// Channel IDs that the key is valid for, for example `msteams`.
    #[serde(default)]
    pub endorsements: Vec<String>,
}

impl SigningKey {
    /// Makes a key.
    #[must_use]
    pub fn new(
        kid: impl Into<String>,
        n: impl Into<String>,
        e: impl Into<String>,
        endorsements: Vec<String>,
    ) -> Self {
        Self {
            kid: kid.into(),
            n: n.into(),
            e: e.into(),
            endorsements,
        }
    }
}

/// A set of signing keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeySet {
    keys: Vec<SigningKey>,
}

impl KeySet {
    /// Makes a set from keys.
    #[must_use]
    pub const fn new(keys: Vec<SigningKey>) -> Self {
        Self { keys }
    }

    /// Reads a JWKS document. It keeps RSA keys with a `kid`, and skips
    /// other keys.
    ///
    /// # Errors
    /// Returns [`TeamsError::Decode`] when the JSON has no `keys` list.
    pub fn from_jwks(jwks: &Value) -> Result<Self, TeamsError> {
        let list = jwks
            .get("keys")
            .and_then(Value::as_array)
            .ok_or_else(|| TeamsError::Decode("jwks has no keys list".to_owned()))?;
        let keys = list
            .iter()
            .filter(|k| k.get("kty").and_then(Value::as_str) == Some("RSA"))
            .filter_map(|k| SigningKey::deserialize(k).ok())
            .collect();
        Ok(Self { keys })
    }

    /// The key with this `kid`.
    #[must_use]
    pub fn get(&self, kid: &str) -> Option<&SigningKey> {
        self.keys.iter().find(|k| k.kid == kid)
    }

    /// Number of keys.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.keys.len()
    }

    /// Returns `true` when the set has no keys.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// Loads the signing keys.
pub trait KeySource: Send + Sync + 'static {
    /// Loads the current key set.
    fn fetch(&self) -> BoxFuture<'_, Result<KeySet, TeamsError>>;
}

/// A fixed key set. Use it in tests, or to pin keys.
#[derive(Debug, Clone)]
pub struct StaticKeys(pub KeySet);

impl KeySource for StaticKeys {
    fn fetch(&self) -> BoxFuture<'_, Result<KeySet, TeamsError>> {
        let keys = self.0.clone();
        Box::pin(async move { Ok(keys) })
    }
}

/// Loads keys from an `OpenID` metadata document: the document gives
/// `jwks_uri`, and `jwks_uri` gives the keys.
pub struct OpenIdKeySource {
    transport: Arc<dyn HttpTransport>,
    metadata_url: String,
}

impl std::fmt::Debug for OpenIdKeySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenIdKeySource")
            .field("metadata_url", &self.metadata_url)
            .finish_non_exhaustive()
    }
}

impl OpenIdKeySource {
    /// Makes a source for this metadata URL.
    #[must_use]
    pub fn new(transport: Arc<dyn HttpTransport>, metadata_url: impl Into<String>) -> Self {
        Self {
            transport,
            metadata_url: metadata_url.into(),
        }
    }
}

impl KeySource for OpenIdKeySource {
    fn fetch(&self) -> BoxFuture<'_, Result<KeySet, TeamsError>> {
        Box::pin(async move {
            let meta = get_json(self.transport.as_ref(), &self.metadata_url).await?;
            let jwks_uri = meta
                .get("jwks_uri")
                .and_then(Value::as_str)
                .filter(|u| crate::config::valid_url(u))
                .ok_or_else(|| TeamsError::Decode("metadata has no https jwks_uri".to_owned()))?;
            KeySet::from_jwks(&get_json(self.transport.as_ref(), jwks_uri).await?)
        })
    }
}

/// GETs a URL and decodes JSON.
async fn get_json(transport: &dyn HttpTransport, url: &str) -> Result<Value, TeamsError> {
    let reply = transport
        .send(HttpRequest::new(Method::Get, url))
        .await
        .map_err(|e| TeamsError::Transport(e.0))?;
    if !(200..300).contains(&reply.status) {
        return Err(TeamsError::Api {
            operation: "openid_keys",
            status: reply.status,
            code: "unknown".to_owned(),
            message: None,
        });
    }
    serde_json::from_slice(&reply.body).map_err(|e| TeamsError::Decode(format!("keys: {e}")))
}

/// What a verified token says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedToken {
    /// The `serviceurl` claim.
    pub service_url: Option<String>,
    /// Endorsements of the signing key.
    pub endorsements: Vec<String>,
}

/// The cached key set.
#[derive(Default)]
struct Cache {
    /// Keys and the time of the load.
    keys: Option<(KeySet, u64)>,
    /// Time of the last failed load.
    last_failure: Option<u64>,
    /// Time of the last load for an unknown `kid`.
    last_unknown_kid: Option<u64>,
}

/// Claims that the check reads.
#[derive(Deserialize)]
struct Claims {
    iss: Option<String>,
    aud: Option<Value>,
    exp: Option<u64>,
    nbf: Option<u64>,
    #[serde(alias = "serviceUrl")]
    serviceurl: Option<String>,
}

/// Rules for the inbound check.
pub(crate) struct VerifierRules {
    pub app_id: String,
    pub issuers: Vec<String>,
    pub clock_skew_secs: u64,
    pub key_cache_secs: u64,
    pub key_min_refresh_secs: u64,
}

/// Checks the `Authorization` header. Caches the keys.
pub(crate) struct Verifier {
    rules: VerifierRules,
    source: Arc<dyn KeySource>,
    metrics: Arc<TeamsMetrics>,
    cache: Mutex<Cache>,
    /// Lets one key refresh run at a time.
    refresh: tokio::sync::Mutex<()>,
}

impl Verifier {
    pub(crate) fn new(
        rules: VerifierRules,
        source: Arc<dyn KeySource>,
        metrics: Arc<TeamsMetrics>,
    ) -> Self {
        Self {
            rules,
            source,
            metrics,
            cache: Mutex::new(Cache::default()),
            refresh: tokio::sync::Mutex::const_new(()),
        }
    }

    /// Checks the header value at `now` (Unix seconds).
    pub(crate) async fn verify(
        &self,
        authorization: Option<&str>,
        now: u64,
    ) -> Result<VerifiedToken, AuthError> {
        let header = authorization.ok_or(AuthError::MissingToken)?;
        if header.len() > MAX_TOKEN_BYTES {
            return Err(AuthError::Malformed);
        }
        // RFC 7235: the scheme is case-insensitive.
        let token = match header.split_once(' ') {
            Some((scheme, t)) if scheme.eq_ignore_ascii_case("bearer") => t.trim(),
            _ => return Err(AuthError::Malformed),
        };
        let head = jsonwebtoken::decode_header(token).map_err(|_| AuthError::Malformed)?;
        // RS256 only. The header cannot pick another algorithm.
        if head.alg != Algorithm::RS256 {
            return Err(AuthError::Algorithm);
        }
        let kid = head.kid.ok_or(AuthError::UnknownKey)?;
        let key = self.key(&kid, now).await?;
        let decoding =
            DecodingKey::from_rsa_components(&key.n, &key.e).map_err(|_| AuthError::Signature)?;
        // The plugin checks the claims itself, with the autumn clock.
        let mut validation = Validation::new(Algorithm::RS256);
        validation.validate_exp = false;
        validation.validate_nbf = false;
        validation.validate_aud = false;
        validation.required_spec_claims.clear();
        let claims = jsonwebtoken::decode::<Claims>(token, &decoding, &validation)
            .map_err(|e| match e.kind() {
                jsonwebtoken::errors::ErrorKind::InvalidSignature => AuthError::Signature,
                _ => AuthError::Malformed,
            })?
            .claims;
        let r = &self.rules;
        if !claims.iss.is_some_and(|i| r.issuers.contains(&i)) {
            return Err(AuthError::Issuer);
        }
        let aud_ok = match &claims.aud {
            Some(Value::String(a)) => *a == r.app_id,
            Some(Value::Array(list)) => list.iter().any(|a| a.as_str() == Some(&r.app_id)),
            _ => false,
        };
        if !aud_ok {
            return Err(AuthError::Audience);
        }
        let exp = claims.exp.ok_or(AuthError::Expired)?;
        if !token_time_valid(now, claims.nbf, exp, r.clock_skew_secs) {
            return Err(AuthError::Expired);
        }
        Ok(VerifiedToken {
            service_url: claims.serviceurl,
            endorsements: key.endorsements,
        })
    }

    /// Looks up `kid` in the cache. Returns the key and `true` when the
    /// cache is fresh.
    fn lookup(&self, kid: &str, now: u64) -> (Option<SigningKey>, bool) {
        let cache = lock(&self.cache);
        cache.keys.as_ref().map_or((None, false), |(set, at)| {
            (
                set.get(kid).cloned(),
                keys_fresh(now, *at, self.rules.key_cache_secs),
            )
        })
    }

    /// Returns the key for `kid`. Loads the key set when the cache is stale,
    /// or when `kid` is unknown. One load runs at a time.
    async fn key(&self, kid: &str, now: u64) -> Result<SigningKey, AuthError> {
        if let (Some(k), true) = self.lookup(kid, now) {
            return Ok(k);
        }
        let _turn = self.refresh.lock().await;
        // Another task can have loaded the keys while this one waited.
        let (found, fresh) = self.lookup(kid, now);
        if let (Some(k), true) = (&found, fresh) {
            return Ok(k.clone());
        }
        let min = self.rules.key_min_refresh_secs;
        let load = {
            let mut cache = lock(&self.cache);
            if fresh {
                // Unknown `kid`: Microsoft can add a key at any time.
                let ok = may_refresh_keys(now, cache.last_unknown_kid, min);
                if ok {
                    cache.last_unknown_kid = Some(now);
                }
                ok
            } else {
                // Stale or empty: load, but wait after a failure.
                may_refresh_keys(now, cache.last_failure, min)
            }
        };
        if load {
            match self.source.fetch().await {
                Ok(set) => {
                    self.metrics.key_refresh("ok");
                    let mut cache = lock(&self.cache);
                    cache.keys = Some((set, now));
                    cache.last_failure = None;
                }
                Err(e) => {
                    // Keep the old keys. A short outage does not stop the bot.
                    self.metrics.key_refresh("error");
                    tracing::warn!(error = %e, "teams: signing key load failed");
                    lock(&self.cache).last_failure = Some(now);
                }
            }
        }
        let cache = lock(&self.cache);
        match &cache.keys {
            Some((set, _)) => set.get(kid).cloned().ok_or(AuthError::UnknownKey),
            None => Err(AuthError::KeysUnavailable),
        }
    }
}

/// Checks the activity against the verified token and the allowlists.
pub(crate) fn check_activity(
    token: &VerifiedToken,
    activity: &Activity,
    allowed_channels: &[String],
    allowed_tenants: &[String],
) -> Result<(), AuthError> {
    let channel = activity
        .channel_id
        .as_deref()
        .ok_or(AuthError::Endorsement)?;
    if !token.endorsements.iter().any(|e| e == channel) {
        return Err(AuthError::Endorsement);
    }
    if !allowed_channels.iter().any(|c| c == channel) {
        return Err(AuthError::Channel);
    }
    match (&token.service_url, &activity.service_url) {
        (Some(claim), Some(url)) if same_service_url(claim, url) => {}
        _ => return Err(AuthError::ServiceUrl),
    }
    if !allowed_tenants.is_empty()
        && !activity
            .tenant_id()
            .is_some_and(|t| allowed_tenants.iter().any(|a| a == t))
    {
        return Err(AuthError::Tenant);
    }
    Ok(())
}

/// Compares two service URLs. Host case and a trailing `/` do not matter.
fn same_service_url(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        url::Url::parse(s).map_or_else(
            |_| s.trim_end_matches('/').to_owned(),
            |u| u.as_str().trim_end_matches('/').to_owned(),
        )
    };
    norm(a) == norm(b)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn token(url: Option<&str>, endorsements: &[&str]) -> VerifiedToken {
        VerifiedToken {
            service_url: url.map(str::to_owned),
            endorsements: endorsements.iter().map(|e| (*e).to_owned()).collect(),
        }
    }

    fn activity() -> Activity {
        let mut a = Activity::message("x");
        a.service_url = Some("https://smba.trafficmanager.net/teams/".into());
        a.channel_id = Some("msteams".into());
        a.channel_data = Some(json!({"tenant": {"id": "t1"}}));
        a
    }

    #[test]
    fn statuses_and_labels() {
        assert_eq!(AuthError::MissingToken.status(), 401);
        assert_eq!(AuthError::Signature.status(), 401);
        assert_eq!(AuthError::Endorsement.status(), 403);
        assert_eq!(AuthError::Tenant.status(), 403);
        assert_eq!(AuthError::KeysUnavailable.status(), 503);
        let all = [
            AuthError::MissingToken,
            AuthError::Malformed,
            AuthError::Algorithm,
            AuthError::UnknownKey,
            AuthError::Signature,
            AuthError::Issuer,
            AuthError::Audience,
            AuthError::Expired,
            AuthError::KeysUnavailable,
            AuthError::Endorsement,
            AuthError::ServiceUrl,
            AuthError::Channel,
            AuthError::Tenant,
        ];
        let labels: std::collections::HashSet<_> = all.iter().map(AuthError::label).collect();
        assert_eq!(labels.len(), all.len(), "labels are unique");
        assert!(all.iter().all(|e| !e.to_string().is_empty()));
    }

    #[test]
    fn check_activity_matrix() {
        let ch = vec!["msteams".to_owned()];
        let any: Vec<String> = Vec::new();
        let url = Some("https://smba.trafficmanager.net/teams/");
        let ok = token(url, &["msteams"]);
        assert_eq!(check_activity(&ok, &activity(), &ch, &any), Ok(()));
        // A trailing slash and case in the host do not matter.
        let mut a = activity();
        a.service_url = Some("https://SMBA.trafficmanager.net/teams".into());
        assert_eq!(check_activity(&ok, &a, &ch, &any), Ok(()));
        assert_eq!(
            check_activity(&token(url, &["skype"]), &activity(), &ch, &any),
            Err(AuthError::Endorsement)
        );
        assert_eq!(
            check_activity(&token(None, &["msteams"]), &activity(), &ch, &any),
            Err(AuthError::ServiceUrl)
        );
        let mut a = activity();
        a.service_url = None;
        assert_eq!(
            check_activity(&ok, &a, &ch, &any),
            Err(AuthError::ServiceUrl)
        );
        let mut a = activity();
        a.channel_id = None;
        assert_eq!(
            check_activity(&ok, &a, &ch, &any),
            Err(AuthError::Endorsement)
        );
        let both = token(url, &["msteams", "webchat"]);
        let mut a = activity();
        a.channel_id = Some("webchat".into());
        assert_eq!(
            check_activity(&both, &a, &ch, &any),
            Err(AuthError::Channel)
        );
        let only = vec!["t1".to_owned()];
        assert_eq!(check_activity(&ok, &activity(), &ch, &only), Ok(()));
        let other = vec!["t2".to_owned()];
        assert_eq!(
            check_activity(&ok, &activity(), &ch, &other),
            Err(AuthError::Tenant)
        );
    }

    #[test]
    fn jwks_parse_skips_unusable_keys() {
        let set = KeySet::from_jwks(&json!({"keys": [
            {"kty": "RSA", "kid": "a", "n": "n1", "e": "AQAB", "endorsements": ["msteams"]},
            {"kty": "RSA", "kid": "b", "n": "n2", "e": "AQAB"},
            {"kty": "EC", "kid": "c"},
            {"kty": "RSA", "n": "n3", "e": "AQAB"},
            "junk",
        ]}))
        .unwrap();
        assert_eq!(set.len(), 2);
        assert!(set.get("b").unwrap().endorsements.is_empty());
        assert!(set.get("c").is_none());
        assert!(!set.is_empty());
        assert!(KeySet::from_jwks(&json!([])).is_err());
        assert!(KeySet::default().is_empty());
    }
}
