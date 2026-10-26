//! Helpers for app tests. Needs the `testing` feature.
//!
//! [`TestAuthority`] acts as the Bot Framework token service. It signs
//! tokens with a fixed test key, and gives the matching key set.
//!
//! ```rust
//! use autumn_plugin_teams::testing::{self, TestAuthority};
//!
//! let authority = TestAuthority::new(testing::APP_ID);
//! let activity = testing::message("hello");
//! let bearer = authority.bearer(&activity, 1_767_225_600);
//! assert!(bearer.starts_with("Bearer "));
//! ```
//!
//! Never trust the test key in production. Anyone can sign with it.
//! See also [`crate::transport::MemoryTransport`].

use std::sync::atomic::{AtomicU64, Ordering};

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{Value, json};

use crate::activity::{Activity, ChannelAccount, ConversationAccount};
use crate::auth::{KeySet, SigningKey, StaticKeys};

/// App ID for tests.
pub const APP_ID: &str = "00000000-0000-0000-0000-00000000a99d";
/// `serviceUrl` for tests.
pub const SERVICE_URL: &str = "https://smba.trafficmanager.net/teams/";
/// Tenant ID for tests.
pub const TENANT_ID: &str = "00000000-0000-0000-0000-0000000000te";
/// Conversation ID for tests.
pub const CONVERSATION_ID: &str = "19:test-conversation@thread.tacv2";
/// Bot ID for tests.
pub const BOT_ID: &str = "28:00000000-0000-0000-0000-00000000a99d";
/// User ID for tests.
pub const USER_ID: &str = "29:test-user";
/// Key ID of the test key.
pub const KEY_ID: &str = "test-key-1";
/// Bot Framework issuer.
pub const ISSUER: &str = "https://api.botframework.com";

/// Test key A (RSA 2048, PKCS #8). Test use only.
const KEY_A_PEM: &str = "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC/BiPvbKboaRDS\nhvRkbbZNioMYW9GHd0JzJ0a2JbRsxnFy8rf/e91rIdjoxGGZxWtrSyxlTmBVZTaj\nlC6n2f+dsgp2qEUMfIagAD9TpMtbohQn5vPPxAY2DQkLHEZD0V58NVWwVcbHG6tO\nHUOEHIsy36fCfV/UOzd3LTkA+0I66ym4F3LwWdXtCaX8yDCG33agoXvT8NVKMwhP\njmfTjngLgooqKR0l2nM0Mwm3i6vN1eTNKQipy5zrXOlv9hKgnTnOb5k8OKlhb/Ti\nIIGZotyp4FmfjHT+XojQMjXXy3MPR2VvQyJt50fSw0016hdfwfQYhaWYhAvK2Km1\nc0El5gffAgMBAAECggEATdLmmQifB1imrd6lerTn7LddqXLe7l28CMYbnOHHm/lN\nBxOzurZwAtGLWWmIielQVioOMEe6H5BYrlQUnfXGmiyK7sSnPOsOvejb4+vjnbVu\nmMMS2bZ3AVZWNNyUE9UctWXVPz3y+fNf2zc45n3qiBNueWIyxHhLpJYb/zZeSToH\nrHbmPY4QhieOQ8ps/OQpa4+AaMTSlWyP8G4KywnYWcJmOXxth2g3eJZFI3eMtLOp\nhUVDz06BzNamQ1WEvQdBQ8aXd/jzoOFIsCc8ZFvngq8i71EESaHM2mi/KoT5ju5V\n3/LlAq7WDP8MCpoJBpzDo56G9KrtqYyXAQl17/zy4QKBgQD9aLTPV9tGDaR+ibf+\nqG6VfXM4qPUofdZXV2Y5R2Bp1Von77jfs7+120G9ZK4gXBIOnXjGSnddMFfE3kv9\nlUT/ub9OIuc7/vVvesT1cZkqOohvnsYkCoRJNvLbUoTcXV/606NUCq2zEYz/O3Sn\nAb3FNsEdWUGu8KEq6ePJHr9VQwKBgQDA+iRxxaG/z4RA6+drSdqh2/O8x5niKa5K\nmSPRMDSenGDC6jgGfSITXQeQa5iMNYKnu6WWR/0kC/BJto+oLT93wp3aby8uxsyA\ntgS2k+GLLQuxaQQa7W1/G41sXIGSyK+lEqlikJjNXS3vFF1lUM5ahdr7WW+E4yto\nuiTl+CqLNQKBgEyt/WBsbl6RTEA5/iIh52x2zIHNpnl9Mnm1aapVbdg4hhJlmKSg\naJdn5EmiCIqkMZaYke42pE3fjf91YLs4Cuvy0e+b4iqmaLA2DxlSKY28roJQIhbP\n092vsecp3igsXRmyJs8MeCkiW270F8VdYoYlnmP6GUUNbUdmBNZb5nuvAoGAd7U0\nEld0hRXnhpZQEh/YPfvBcOOos8EDW0cwDr4GhjWfWIoOgwfwsW5OGOqF0MQI7p/a\nkiTKBShawH89EigvbM0POscpz6yGg3ll5xRJHxqsXDs2zrVl7SlcS1H/FtzjCFVR\nOdFU2F/q9Jn/ba/4sfLC6pSKNzbsYKTH6thNG9kCgYEAiXUPGew+rmZRKvA5YrWT\nLkgvphbmUkQlAhHB4GroAPMOYz/K7wuhLJ0mPYTGACuM49DqMDQzALJ5ZkyPuQWR\ngPwuGWrBU0FS5TGkLdDFTFpoyqqBldGW//5kOVKWwQ6zwppblnqNXmTd3iX/G1pp\nzNTPcDHYy3GALUHsMYGsnc8=\n-----END PRIVATE KEY-----";
/// Modulus of key A, base64url.
const KEY_A_N: &str = "vwYj72ym6GkQ0ob0ZG22TYqDGFvRh3dCcydGtiW0bMZxcvK3_3vdayHY6MRhmcVra0ssZU5gVWU2o5Qup9n_nbIKdqhFDHyGoAA_U6TLW6IUJ-bzz8QGNg0JCxxGQ9FefDVVsFXGxxurTh1DhByLMt-nwn1f1Ds3dy05APtCOuspuBdy8FnV7Qml_Mgwht92oKF70_DVSjMIT45n0454C4KKKikdJdpzNDMJt4urzdXkzSkIqcuc61zpb_YSoJ05zm-ZPDipYW_04iCBmaLcqeBZn4x0_l6I0DI118tzD0dlb0MibedH0sNNNeoXX8H0GIWlmIQLytiptXNBJeYH3w";
/// Test key B: a second key for forgery tests. Test use only.
const KEY_B_PEM: &str = "-----BEGIN PRIVATE KEY-----\nMIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQC0hF1NypInMoAw\nlRcv1JvUydAS6YzSpXUN+Y6dFVpoalQbnSP42ipEDZSSlhfzcSWyT/1djE2kEQvs\nktXfL6A1VNaSVAiLV4lvaZOpW6ShiXokjalTiMWkECPmCg8njJwumV/aWLYDkdKu\njzIpLKTu8+wJ8AgC+seV8nwEqeDs54YS7oRfGXkj6mtAaL+aWzf8Thw82qdfdMms\n5TdrK79M6WoEl5ktYtFbQVPh7h+KlGJMwW020XrfokIRamBl2q3dIIMlU/0vXWd1\nK+pVX9rXiMbV0lyuJg+HFtdcpwnFu4t66ZPrhTv4yMmqMU7RBxx7C+TFAiQx7AjL\nEoukoNbvAgMBAAECggEACCnmSFLYGj1emRPdA7+TOTkpQk8H8dd5ZuOO2gwcVcAy\nmTnEOzMD71Dyk6XbDm+NsLXiXkfZPl67pybFf6hD5WA2q1Eb6SVRJKs7JNEYAxWD\nDcJ5p7qglDiwpEIh3lGDF1kTVyi6OTNv5OS6ntcTMcVGkEp9KkP/dl4MKBqAqWYE\nQA9DLsJgWiD9b+bP29MillbLsKmopvCJHUXq7yOEaMfR1Sn5DRMn/iGilDCmXXuw\nkwUv0i03yqgT+0bE+jW88la13Pw8EMWKJVxb093GM/EqT1IJ0vB5gVj9f1J4Gfn1\nua9j4lnUZln16ofszzgRgnYSwy8GSCDoFNQOi2A0tQKBgQDkDslCxhk+mo0GZwmu\n4BbEea3zYOYeW7tJm12m2z5gQXvtEEz5+y+OiRmMUyev5oWOu+DQyf59obLQ73Wx\nGQZfN5+XItsK8plhi+pTPEyBG78ccc7ncWviVtWnGLJj1ucjYJ8pLRdCXXgMl4qu\nQxbvWieq/0BRYRSWWMaHPaUtBQKBgQDKom0Kmput0UVcmGVn4deGqKtbZFbjgyPH\nkwuoIzqdZVLrCka3Z9mKbe91odC9G36w5ay6XZ5u2Yzrc6FPsxe5NjXyGLhIgrwG\nsncrXo4gRpa52lwfmUQqLkS26TF2Zx2QHERvzgQhS2qgunKDjIYH6bDiq5bxBPAh\nz1MrIfoWYwKBgHWw/8I68zowjgi40fFyaa15UgDgh8Ew+8UvrbP3Pwhzot2jZOY4\n78DyFcIqA7guEcyoPovhQjlZYiua+DStwnEPBof7j+jla6xAhWvxGVYjmIu4W9tq\n2A8VWusBvsXxWJUUPu8D+xXfoJ128zWdH8/jycbgNTQKEpLIO6P9dyl1AoGALlN6\nCG2WX+OzLtsse9Z3FrMjEe7bqoIFwQ/OH2+GbAuJLHEZ/74/TtWZZFRNS56olTq9\nBZJ3sTINdJrn6zevmclaBfI+110ELafJdOHWLmzU8sKIQzUgD2CqlwKzKzWh3ZuI\nzeLUsOP0CbD2kzqiPdbgeMHv7URKvgAPLJJFAa8CgYAuljnUv1dppa+UhRc2W/0d\neG0GEFI8ooDgGSoNqqRHjlaiAa49W5Zv8eEhuHXiQ3nZmTDL4E5hjW23lwor6R4M\nebZb6a5TY5x29bMLSVjTQ6ly5ditfoLsQYtgoZCJsrGPodBYm4p1Obe3rxsVH/t0\n62izUTne8WQ0NJ3kJ2ZIOw==\n-----END PRIVATE KEY-----";
/// Modulus of key B, base64url.
const KEY_B_N: &str = "tIRdTcqSJzKAMJUXL9Sb1MnQEumM0qV1DfmOnRVaaGpUG50j-NoqRA2UkpYX83Elsk_9XYxNpBEL7JLV3y-gNVTWklQIi1eJb2mTqVukoYl6JI2pU4jFpBAj5goPJ4ycLplf2li2A5HSro8yKSyk7vPsCfAIAvrHlfJ8BKng7OeGEu6EXxl5I-prQGi_mls3_E4cPNqnX3TJrOU3ayu_TOlqBJeZLWLRW0FT4e4fipRiTMFtNtF636JCEWpgZdqt3SCDJVP9L11ndSvqVV_a14jG1dJcriYPhxbXXKcJxbuLeumT64U7-MjJqjFO0QccewvkxQIkMewIyxKLpKDW7w";
/// RSA exponent 65537, base64url.
const EXPONENT: &str = "AQAB";

/// Signs Bot Framework style tokens with a fixed test key.
#[derive(Clone)]
pub struct TestAuthority {
    app_id: String,
    kid: String,
    pem: &'static str,
    n: &'static str,
    endorsements: Vec<String>,
}

impl std::fmt::Debug for TestAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestAuthority")
            .field("app_id", &self.app_id)
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

impl TestAuthority {
    /// An authority for `app_id` with key A, `kid` [`KEY_ID`], and the
    /// endorsement `msteams`.
    #[must_use]
    pub fn new(app_id: &str) -> Self {
        Self {
            app_id: app_id.to_owned(),
            kid: KEY_ID.to_owned(),
            pem: KEY_A_PEM,
            n: KEY_A_N,
            endorsements: vec!["msteams".to_owned()],
        }
    }

    /// An attacker: the same `kid` as [`Self::new`], but key B. Its tokens
    /// fail the signature check.
    #[must_use]
    pub fn rogue(app_id: &str) -> Self {
        Self {
            pem: KEY_B_PEM,
            n: KEY_B_N,
            ..Self::new(app_id)
        }
    }

    /// Uses this key ID.
    #[must_use]
    pub fn with_kid(mut self, kid: &str) -> Self {
        kid.clone_into(&mut self.kid);
        self
    }

    /// Uses these endorsements in the key set.
    #[must_use]
    pub fn with_endorsements(mut self, endorsements: &[&str]) -> Self {
        self.endorsements = endorsements.iter().map(|e| (*e).to_owned()).collect();
        self
    }

    /// The public key, as a JWKS entry.
    #[must_use]
    pub fn signing_key(&self) -> SigningKey {
        SigningKey::new(
            self.kid.clone(),
            self.n,
            EXPONENT,
            self.endorsements.clone(),
        )
    }

    /// A key source with only this key.
    #[must_use]
    pub fn key_source(&self) -> StaticKeys {
        StaticKeys(KeySet::new(vec![self.signing_key()]))
    }

    /// The key set as a JWKS JSON document.
    #[must_use]
    pub fn jwks(&self) -> Value {
        json!({ "keys": [{
            "kty": "RSA",
            "use": "sig",
            "kid": self.kid,
            "n": self.n,
            "e": EXPONENT,
            "endorsements": self.endorsements,
        }] })
    }

    /// Valid claims at `now` (Unix seconds) for this `serviceUrl`. Change
    /// them and sign with [`Self::sign`] to test a bad token.
    #[must_use]
    pub fn claims(&self, now: u64, service_url: &str) -> Value {
        json!({
            "iss": ISSUER,
            "aud": self.app_id,
            "nbf": now.saturating_sub(60),
            "iat": now.saturating_sub(60),
            "exp": now.saturating_add(3_600),
            "serviceurl": service_url,
        })
    }

    /// Signs `claims` with RS256 and this key.
    #[must_use]
    pub fn sign(&self, claims: &Value) -> String {
        self.sign_with(Algorithm::RS256, claims)
    }

    /// Signs `claims` with `alg` and this key. Use it to test that the
    /// plugin refuses other RSA algorithms.
    ///
    /// # Panics
    /// When `alg` is not an RSA algorithm.
    #[must_use]
    #[allow(clippy::expect_used)] // The test keys are fixed and valid.
    pub fn sign_with(&self, alg: Algorithm, claims: &Value) -> String {
        let mut header = Header::new(alg);
        header.kid = Some(self.kid.clone());
        let key = EncodingKey::from_rsa_pem(self.pem.as_bytes()).expect("test key is valid");
        jsonwebtoken::encode(&header, claims, &key).expect("test key signs")
    }

    /// `Bearer <token>` for `activity` at `now`: valid claims, with the
    /// activity `serviceUrl`.
    #[must_use]
    pub fn bearer(&self, activity: &Activity, now: u64) -> String {
        let url = activity.service_url.as_deref().unwrap_or(SERVICE_URL);
        format!("Bearer {}", self.sign(&self.claims(now, url)))
    }
}

/// An inbound activity of `kind` with Teams fields set: a new `id` for
/// each call, `serviceUrl`, `channelId`, `from`, `recipient`,
/// `conversation`, and the tenant.
#[must_use]
pub fn inbound(kind: &str) -> Activity {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut conversation = ConversationAccount::new(CONVERSATION_ID);
    conversation.tenant_id = Some(TENANT_ID.to_owned());
    conversation.conversation_type = Some("personal".to_owned());
    Activity {
        kind: kind.to_owned(),
        id: Some(format!("test-{id}")),
        service_url: Some(SERVICE_URL.to_owned()),
        channel_id: Some("msteams".to_owned()),
        from: Some(ChannelAccount::new(USER_ID, Some("Ada".to_owned()))),
        recipient: Some(ChannelAccount::new(BOT_ID, Some("Bot".to_owned()))),
        conversation: Some(conversation),
        channel_data: Some(json!({ "tenant": { "id": TENANT_ID } })),
        ..Activity::default()
    }
}

/// An inbound message with this text.
#[must_use]
pub fn message(text: &str) -> Activity {
    Activity {
        text: Some(text.to_owned()),
        ..inbound(crate::activity::types::MESSAGE)
    }
}

/// An inbound invoke with this name and value.
#[must_use]
pub fn invoke(name: &str, value: Value) -> Activity {
    Activity {
        name: Some(name.to_owned()),
        value: Some(value),
        ..inbound(crate::activity::types::INVOKE)
    }
}

/// An inbound `adaptiveCard/action` invoke with this verb and data.
#[must_use]
pub fn card_action(verb: &str, data: Value) -> Activity {
    let mut value = json!({ "action": { "type": "Action.Execute", "verb": verb } });
    value["action"]["data"] = data;
    invoke(crate::activity::ADAPTIVE_CARD_ACTION, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_distinct_and_tokens_have_the_kid() {
        assert_ne!(KEY_A_N, KEY_B_N);
        let a = TestAuthority::new(APP_ID);
        let t = a.sign(&a.claims(100, SERVICE_URL));
        let h = jsonwebtoken::decode_header(&t).unwrap();
        assert_eq!(h.kid.as_deref(), Some(KEY_ID));
        assert_eq!(h.alg, Algorithm::RS256);
        assert_eq!(a.jwks()["keys"][0]["kid"], KEY_ID);
        assert!(format!("{a:?}").contains("TestAuthority"));
        assert!(!format!("{a:?}").contains("PRIVATE"));
    }
}
