//! `[teams]` configuration.
//!
//! Sources, in order (later wins):
//! 1. `[teams]` in `autumn.toml`.
//! 2. `[teams]` in the profile file, for example `autumn-prod.toml`.
//! 3. `.env` values, then the process environment: `AUTUMN_TEAMS__<PATH>`,
//!    for example `AUTUMN_TEAMS__API__MAX_ATTEMPTS=5`. In an env var, put a
//!    comma between list items.
//!
//! The plugin does not read `[profile.<name>.teams]` in `autumn.toml`. With
//! `[server] strict_config = true`, autumn stops at boot on that section.
//!
//! Secrets are not in the config. The config holds the names of the env vars
//! that hold them.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::TeamsError;
use crate::policy::{DEFAULT_CLOCK_SKEW_SECS, MAX_ATTEMPTS, MAX_KEY_AGE_SECS, RetryRule};

/// Name of the TOML section.
pub const SECTION: &str = "teams";
/// Prefix for environment overrides.
pub const ENV_PREFIX: &str = "AUTUMN_TEAMS__";
/// Teams waits 5 s for an invoke reply, network time included. The invoke
/// timeout keeps 500 ms or more for the network.
pub const MAX_INVOKE_TIMEOUT_MS: u64 = 4_500;
/// The tenant for a multi-tenant bot token.
pub const MULTI_TENANT: &str = "botframework.com";

/// Plugin configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct TeamsConfig {
    /// Env var with the Microsoft app ID of the bot.
    pub app_id_env: String,
    /// Env var with the app password (client secret).
    pub app_password_env: String,
    /// Env var with the Entra tenant ID of a single-tenant bot. When it is
    /// not set, the bot is multi-tenant.
    pub tenant_id_env: String,
    /// Login base URL. It ends with `/`.
    pub login_base_url: String,
    /// OAuth scope of the outbound token.
    pub oauth_scope: String,
    /// `OpenID` metadata URL of the inbound token issuer.
    pub openid_metadata_url: String,
    /// Accepted `iss` values of inbound tokens.
    pub issuers: Vec<String>,
    /// Clock skew for `exp` and `nbf`, 0 to 600 s.
    pub clock_skew_secs: u64,
    /// Hosts that a `serviceUrl` can use. HTTPS only.
    pub service_url_hosts: Vec<String>,
    /// Accepted activity `channelId` values.
    pub allowed_channels: Vec<String>,
    /// Accepted tenant IDs. Empty: all tenants.
    pub allowed_tenants: Vec<String>,
    /// Time to wait for an invoke handler before an empty 200,
    /// 1 to 4500 ms.
    pub invoke_timeout_ms: u64,
    /// Largest request body.
    pub max_body_bytes: usize,
    /// Time to remember an activity ID, to drop retries.
    pub dedup_window_secs: u64,
    /// Time to wait for running handlers at shutdown.
    pub drain_timeout_secs: u64,
    /// Reply text when a message handler fails. No error detail goes to Teams.
    pub error_text: String,
    /// Inbound key settings.
    pub keys: KeysConfig,
    /// Connector client settings.
    pub api: ApiConfig,
    /// Health indicator settings.
    pub health: HealthConfig,
}

impl Default for TeamsConfig {
    fn default() -> Self {
        Self {
            app_id_env: "TEAMS_APP_ID".to_owned(),
            app_password_env: "TEAMS_APP_PASSWORD".to_owned(),
            tenant_id_env: "TEAMS_TENANT_ID".to_owned(),
            login_base_url: "https://login.microsoftonline.com/".to_owned(),
            oauth_scope: "https://api.botframework.com/.default".to_owned(),
            openid_metadata_url:
                "https://login.botframework.com/v1/.well-known/openidconfiguration".to_owned(),
            issuers: vec!["https://api.botframework.com".to_owned()],
            clock_skew_secs: DEFAULT_CLOCK_SKEW_SECS,
            service_url_hosts: vec!["smba.trafficmanager.net".to_owned()],
            allowed_channels: vec!["msteams".to_owned()],
            allowed_tenants: Vec::new(),
            invoke_timeout_ms: 4_000,
            max_body_bytes: 1_048_576,
            dedup_window_secs: 3_600,
            drain_timeout_secs: 10,
            error_text: "Sorry, that did not work. Try again later.".to_owned(),
            keys: KeysConfig::default(),
            api: ApiConfig::default(),
            health: HealthConfig::default(),
        }
    }
}

/// Inbound key settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct KeysConfig {
    /// Time to keep the key set, 60 to 86400 s.
    pub cache_secs: u64,
    /// Smallest time between two refreshes for an unknown `kid`, 1 to 3600 s.
    pub min_refresh_secs: u64,
}

impl Default for KeysConfig {
    fn default() -> Self {
        Self {
            cache_secs: MAX_KEY_AGE_SECS,
            min_refresh_secs: 300,
        }
    }
}

/// Connector client settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct ApiConfig {
    /// Attempts per call, 1 to 10.
    pub max_attempts: u32,
    /// First backoff for 5xx and network errors.
    pub initial_backoff_ms: u64,
    /// Largest wait before a retry. A longer `Retry-After` stops the call.
    pub max_wait_ms: u64,
    /// Time limit for one HTTP attempt.
    pub timeout_ms: u64,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff_ms: 500,
            max_wait_ms: 30_000,
            timeout_ms: 10_000,
        }
    }
}

impl ApiConfig {
    /// The retry rule for the policy core.
    #[must_use]
    pub const fn rule(&self) -> RetryRule {
        RetryRule {
            max_attempts: self.max_attempts,
            initial_backoff_ms: self.initial_backoff_ms,
            max_wait_ms: self.max_wait_ms,
        }
    }
}

/// Health indicator settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct HealthConfig {
    /// Time to keep an `Up` result. A `Down` result stays 5 s or less.
    pub cache_secs: u64,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self { cache_secs: 30 }
    }
}

impl TeamsConfig {
    /// Reads `[teams]` from the text of an `autumn.toml` file.
    ///
    /// # Errors
    /// Returns [`TeamsError::Config`] for bad TOML, unknown keys, or failed
    /// validation.
    pub fn from_toml_str(autumn_toml: &str) -> Result<Self, TeamsError> {
        Self::from_layers(Some(autumn_toml), None, std::iter::empty())
    }

    /// Merges, in order: `[teams]` in `base`, `[teams]` in `profile_file`,
    /// and env vars. Then validates.
    fn from_layers(
        base: Option<&str>,
        profile_file: Option<&str>,
        env: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, TeamsError> {
        let parse = |text: &str| -> Result<toml::Table, TeamsError> {
            toml::from_str(text).map_err(|e| TeamsError::Config(format!("toml: {e}")))
        };
        let mut table = toml::Table::new();
        for text in [base, profile_file].into_iter().flatten() {
            if let Some(toml::Value::Table(section)) = parse(text)?.get(SECTION) {
                merge(&mut table, section.clone());
            }
        }
        let schema = toml::Table::try_from(Self::default())
            .map_err(|e| TeamsError::Config(e.to_string()))?;
        for (key, value) in env {
            let Some(path) = key.strip_prefix(ENV_PREFIX) else {
                continue;
            };
            let path: Vec<String> = path.split("__").map(str::to_ascii_lowercase).collect();
            // The error names the variable, never the value.
            let typed = typed_env_value(&schema, &path, &value)
                .ok_or_else(|| TeamsError::Config(format!("{key}: value is not valid")))?;
            insert_path(&mut table, &path, typed);
        }
        let cfg: Self = toml::Value::Table(table)
            .try_into()
            .map_err(|e| TeamsError::Config(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Loads config from files in `manifest_dir` (else the current
    /// directory) and from `env`.
    ///
    /// # Errors
    /// Returns [`TeamsError::Config`] for a file that cannot be read or
    /// parsed, or for failed validation.
    pub fn load_from_dir(
        manifest_dir: &Path,
        profile_names: &[String],
        env: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, TeamsError> {
        let find = |file: &str| {
            let candidate = manifest_dir.join(file);
            if candidate.exists() {
                candidate
            } else {
                PathBuf::from(file)
            }
        };
        let base = read_optional(&find("autumn.toml"))?;
        let mut profile = None;
        for name in profile_names {
            if let Some(text) = read_optional(&find(&format!("autumn-{name}.toml")))? {
                profile = Some(text);
                break;
            }
        }
        Self::from_layers(base.as_deref(), profile.as_deref(), env)
    }

    /// Loads config like autumn-web does: `$AUTUMN_MANIFEST_DIR` (else `.`),
    /// the app profile, `.env`, and the process environment.
    ///
    /// # Errors
    /// Returns [`TeamsError::Config`] when loading or validation fails.
    pub fn load(profile: Option<&str>) -> Result<Self, TeamsError> {
        use autumn_web::config::Env as _;
        let os = autumn_web::config::OsEnv;
        let names = profile
            .map(|p| {
                // Same selector order as autumn-web: env vars, then `--profile`.
                let selector = ["AUTUMN_ENV", "AUTUMN_PROFILE"]
                    .iter()
                    .find_map(|k| os.var(k).ok().filter(|v| !v.trim().is_empty()))
                    .or_else(profile_flag)
                    .map_or_else(|| p.to_owned(), |v| v.trim().to_owned());
                autumn_web::config::profile_override_file_lookup_names(p, &selector)
            })
            .unwrap_or_default();
        let dir = os
            .var("AUTUMN_MANIFEST_DIR")
            .map_or_else(|_| PathBuf::from("."), PathBuf::from);
        Self::load_from_dir(&dir, &names, process_env()?)
    }

    /// Checks the values.
    ///
    /// # Errors
    /// Returns [`TeamsError::Config`] with the first problem found.
    pub fn validate(&self) -> Result<(), TeamsError> {
        let bad = |m: String| Err(TeamsError::Config(m));
        for (name, value) in [
            ("app_id_env", &self.app_id_env),
            ("app_password_env", &self.app_password_env),
            ("tenant_id_env", &self.tenant_id_env),
            ("oauth_scope", &self.oauth_scope),
            ("error_text", &self.error_text),
        ] {
            if value.trim().is_empty() {
                return bad(format!("{name} must not be empty"));
            }
        }
        if !valid_url(&self.login_base_url) || !self.login_base_url.ends_with('/') {
            return bad(format!(
                "login_base_url must be an https URL that ends with / (http only on loopback): {}",
                self.login_base_url
            ));
        }
        if !valid_url(&self.openid_metadata_url) {
            return bad(format!(
                "openid_metadata_url must be an https URL (http only on loopback): {}",
                self.openid_metadata_url
            ));
        }
        for (name, list) in [
            ("issuers", &self.issuers),
            ("allowed_channels", &self.allowed_channels),
        ] {
            if list.is_empty() || list.iter().any(|v| v.trim().is_empty()) {
                return bad(format!("{name} must list one or more values"));
            }
        }
        if self.allowed_tenants.iter().any(|v| v.trim().is_empty()) {
            return bad("allowed_tenants must not hold an empty value".to_owned());
        }
        if self.service_url_hosts.is_empty()
            || self
                .service_url_hosts
                .iter()
                .any(|h| h.trim().is_empty() || h.contains(['/', ':', '@']))
        {
            return bad("service_url_hosts must list one or more host names".to_owned());
        }
        if self.clock_skew_secs > 600 {
            return bad(format!(
                "clock_skew_secs must be 0 to 600, not {}",
                self.clock_skew_secs
            ));
        }
        if !(1..=MAX_INVOKE_TIMEOUT_MS).contains(&self.invoke_timeout_ms) {
            return bad(format!(
                "invoke_timeout_ms must be 1 to {MAX_INVOKE_TIMEOUT_MS}, not {}",
                self.invoke_timeout_ms
            ));
        }
        if self.max_body_bytes == 0 {
            return bad("max_body_bytes must be 1 or more".to_owned());
        }
        if !(60..=MAX_KEY_AGE_SECS).contains(&self.keys.cache_secs) {
            return bad(format!(
                "keys.cache_secs must be 60 to {MAX_KEY_AGE_SECS}, not {}",
                self.keys.cache_secs
            ));
        }
        if !(1..=3_600).contains(&self.keys.min_refresh_secs) {
            return bad(format!(
                "keys.min_refresh_secs must be 1 to 3600, not {}",
                self.keys.min_refresh_secs
            ));
        }
        if !(1..=MAX_ATTEMPTS).contains(&self.api.max_attempts) {
            return bad(format!(
                "api.max_attempts must be 1 to {MAX_ATTEMPTS}, not {}",
                self.api.max_attempts
            ));
        }
        if self.api.timeout_ms == 0 {
            return bad("api.timeout_ms must be 1 or more".to_owned());
        }
        Ok(())
    }

    /// The full token endpoint URL for this tenant.
    pub(crate) fn token_url(&self, tenant_id: Option<&str>) -> String {
        format!(
            "{}{}/oauth2/v2.0/token",
            self.login_base_url,
            tenant_id.unwrap_or(MULTI_TENANT)
        )
    }
}

/// Reads one variable from the process environment, else from `.env`.
pub(crate) fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().or_else(|| {
        autumn_web::dotenv::resolve_process_dotenv()
            .ok()?
            .into_iter()
            .find_map(|(k, v)| (k == name).then_some(v))
    })
}

/// Returns `.env` values, then the process environment (later wins).
///
/// It skips a variable whose name or value is not UTF-8.
fn process_env() -> Result<Vec<(String, String)>, TeamsError> {
    let mut vars = autumn_web::dotenv::resolve_process_dotenv()
        .map_err(|e| TeamsError::Config(format!(".env: {e}")))?;
    vars.extend(
        std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))),
    );
    Ok(vars)
}

/// The value of `--profile <name>` or `--profile=<name>` in the process args.
fn profile_flag() -> Option<String> {
    let args: Vec<String> = std::env::args_os()
        .filter_map(|a| a.into_string().ok())
        .collect();
    args.iter()
        .enumerate()
        .find_map(|(i, a)| {
            a.strip_prefix("--profile=").map(str::to_owned).or_else(|| {
                (a == "--profile")
                    .then(|| args.get(i + 1).cloned())
                    .flatten()
            })
        })
        // Like autumn-web: an empty value selects nothing.
        .filter(|p| !p.trim().is_empty())
}

/// An `https` URL, or `http` on a loopback host.
pub(crate) fn valid_url(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    u.scheme() == "https" || (u.scheme() == "http" && is_loopback(u.host().as_ref()))
}

/// Returns `true` for `localhost` and loopback IP hosts.
pub(crate) fn is_loopback(host: Option<&url::Host<&str>>) -> bool {
    match host {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

fn read_optional(path: &Path) -> Result<Option<String>, TeamsError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(TeamsError::Config(format!("{}: {e}", path.display()))),
    }
}

/// Deep merge: tables merge by key; other values replace.
fn merge(into: &mut toml::Table, from: toml::Table) {
    for (key, value) in from {
        match (into.get_mut(&key), value) {
            (Some(toml::Value::Table(dst)), toml::Value::Table(src)) => merge(dst, src),
            (_, value) => {
                into.insert(key, value);
            }
        }
    }
}

fn insert_path(table: &mut toml::Table, path: &[String], value: toml::Value) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut cur = table;
    for key in parents {
        let entry = cur
            .entry(key.clone())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if !entry.is_table() {
            *entry = toml::Value::Table(toml::Table::new());
        }
        let toml::Value::Table(next) = entry else {
            return;
        };
        cur = next;
    }
    cur.insert(last.clone(), value);
}

/// Converts an env value to the type of the default at the same path. A list
/// is a comma-separated string.
fn typed_env_value(schema: &toml::Table, path: &[String], raw: &str) -> Option<toml::Value> {
    let mut node: Option<&toml::Value> = None;
    let mut table = Some(schema);
    for key in path {
        node = table.and_then(|t| t.get(key));
        table = node.and_then(toml::Value::as_table);
    }
    match node {
        Some(toml::Value::Integer(_)) => raw.trim().parse().ok().map(toml::Value::Integer),
        Some(toml::Value::Boolean(_)) => raw.trim().parse().ok().map(toml::Value::Boolean),
        Some(toml::Value::Array(_)) => Some(toml::Value::Array(
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| toml::Value::String(s.to_owned()))
                .collect(),
        )),
        _ => Some(toml::Value::String(raw.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn defaults_are_valid() {
        TeamsConfig::default().validate().unwrap();
        assert_eq!(
            TeamsConfig::from_toml_str("").unwrap(),
            TeamsConfig::default()
        );
    }

    #[test]
    fn reads_section_and_rejects_unknown_keys() {
        let c = TeamsConfig::from_toml_str(
            "[teams]\ninvoke_timeout_ms = 900\nallowed_tenants = [\"t1\"]\n[teams.api]\nmax_attempts = 1\n",
        )
        .unwrap();
        assert_eq!(c.invoke_timeout_ms, 900);
        assert_eq!(c.allowed_tenants, ["t1"]);
        assert_eq!(c.api.max_attempts, 1);
        assert!(TeamsConfig::from_toml_str("[teams]\ninvoke_timeout = 9\n").is_err());
        assert!(TeamsConfig::from_toml_str("[teams\n").is_err());
        // Other sections are not ours.
        TeamsConfig::from_toml_str("[server]\nport = 1\n").unwrap();
    }

    #[test]
    fn env_overlay_types_values() {
        let c = TeamsConfig::from_layers(
            None,
            None,
            env(&[
                ("AUTUMN_TEAMS__INVOKE_TIMEOUT_MS", "100"),
                ("AUTUMN_TEAMS__APP_ID_ENV", "MY_ID"),
                ("AUTUMN_TEAMS__ALLOWED_TENANTS", "a, b"),
                ("AUTUMN_TEAMS__KEYS__MIN_REFRESH_SECS", "5"),
                ("OTHER", "x"),
            ]),
        )
        .unwrap();
        assert_eq!(c.invoke_timeout_ms, 100);
        assert_eq!(c.app_id_env, "MY_ID");
        assert_eq!(c.allowed_tenants, ["a", "b"]);
        assert_eq!(c.keys.min_refresh_secs, 5);
        let e = TeamsConfig::from_layers(
            None,
            None,
            env(&[("AUTUMN_TEAMS__CLOCK_SKEW_SECS", "soon")]),
        )
        .unwrap_err();
        assert!(
            e.to_string().contains("AUTUMN_TEAMS__CLOCK_SKEW_SECS"),
            "{e}"
        );
        assert!(!e.to_string().contains("soon"), "{e}");
    }

    #[test]
    fn validation_ranges() {
        let check = |f: fn(&mut TeamsConfig)| {
            let mut c = TeamsConfig::default();
            f(&mut c);
            c.validate().unwrap_err().to_string()
        };
        assert!(check(|c| c.invoke_timeout_ms = 0).contains("invoke_timeout_ms"));
        assert!(check(|c| c.invoke_timeout_ms = 4_501).contains("invoke_timeout_ms"));
        assert!(check(|c| c.clock_skew_secs = 601).contains("clock_skew_secs"));
        assert!(check(|c| c.keys.cache_secs = 59).contains("keys.cache_secs"));
        assert!(check(|c| c.keys.cache_secs = MAX_KEY_AGE_SECS + 1).contains("keys.cache_secs"));
        assert!(check(|c| c.keys.min_refresh_secs = 0).contains("keys.min_refresh_secs"));
        assert!(check(|c| c.keys.min_refresh_secs = 3_601).contains("keys.min_refresh_secs"));
        assert!(check(|c| c.api.max_attempts = 0).contains("max_attempts"));
        assert!(check(|c| c.api.max_attempts = MAX_ATTEMPTS + 1).contains("max_attempts"));
        assert!(check(|c| c.api.timeout_ms = 0).contains("timeout_ms"));
        assert!(check(|c| c.login_base_url = "ftp://x/".into()).contains("login_base_url"));
        assert!(
            check(|c| c.login_base_url = "https://login.example".into()).contains("login_base_url")
        );
        assert!(
            check(|c| c.openid_metadata_url = "http://x.example/m".into())
                .contains("openid_metadata_url")
        );
        assert!(check(|c| c.issuers.clear()).contains("issuers"));
        assert!(check(|c| c.issuers = vec![" ".into()]).contains("issuers"));
        assert!(check(|c| c.service_url_hosts.clear()).contains("service_url_hosts"));
        assert!(check(|c| c.service_url_hosts = vec!["a/b".into()]).contains("service_url_hosts"));
        assert!(check(|c| c.allowed_channels.clear()).contains("allowed_channels"));
        assert!(check(|c| c.allowed_tenants = vec![String::new()]).contains("allowed_tenants"));
        assert!(check(|c| c.max_body_bytes = 0).contains("max_body_bytes"));
        assert!(check(|c| c.app_id_env.clear()).contains("app_id_env"));
        assert!(check(|c| c.app_password_env.clear()).contains("app_password_env"));
        assert!(check(|c| c.tenant_id_env.clear()).contains("tenant_id_env"));
        assert!(check(|c| c.oauth_scope.clear()).contains("oauth_scope"));
        assert!(check(|c| c.error_text.clear()).contains("error_text"));
    }

    #[test]
    fn inline_profile_section_is_not_read() {
        let base = "[teams]\ninvoke_timeout_ms = 1\n[profile.prod.teams]\ninvoke_timeout_ms = 3\n";
        let c = TeamsConfig::from_toml_str(base).unwrap();
        assert_eq!(c.invoke_timeout_ms, 1);
    }

    #[test]
    fn url_rules() {
        for ok in [
            "https://login.microsoftonline.com/",
            "http://127.0.0.1:9/x/",
            "http://localhost/x",
            "http://[::1]:8/x/",
        ] {
            assert!(valid_url(ok), "{ok}");
        }
        for bad in [
            "http://login.example/",
            "http://10.0.0.1/",
            "ftp://x/",
            "nope",
        ] {
            assert!(!valid_url(bad), "{bad}");
        }
    }
}
