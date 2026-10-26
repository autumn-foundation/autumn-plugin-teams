//! Pure policy core. Verified with Verus in `verus/policy.rs`.
//!
//! Keep this file and the Verus file in step.

/// Bot Framework clock skew: 5 minutes.
pub const DEFAULT_CLOCK_SKEW_SECS: u64 = 300;
/// Refresh the key set at least once every 24 hours.
pub const MAX_KEY_AGE_SECS: u64 = 86_400;
/// Get a new outbound token this many seconds before it expires.
pub const TOKEN_REFRESH_MARGIN_SECS: u64 = 300;
/// Largest number of attempts for one Connector call.
pub const MAX_ATTEMPTS: u32 = 10;
/// Wait for a 429 with no `Retry-After` header.
pub const DEFAULT_RETRY_AFTER_MS: u64 = 1_000;

/// Returns `true` when a token with `exp` and `nbf` is valid at `now`.
#[must_use]
pub const fn token_time_valid(now: u64, nbf: Option<u64>, exp: u64, skew: u64) -> bool {
    // Compare without overflow: `now - exp <= skew` only when `now > exp`.
    let exp_ok = now <= exp || now - exp <= skew;
    let nbf_ok = match nbf {
        Some(n) => n <= now || n - now <= skew,
        None => true,
    };
    exp_ok && nbf_ok
}

/// Returns `true` when the cached token must be replaced at `now`.
#[must_use]
pub const fn token_needs_refresh(now: u64, expires_at: u64, margin: u64) -> bool {
    expires_at <= now || expires_at - now <= margin
}

/// Returns `now + expires_in`. It saturates.
#[must_use]
pub const fn token_expires_at(now: u64, expires_in: u64) -> u64 {
    if expires_in <= u64::MAX - now {
        now + expires_in
    } else {
        u64::MAX
    }
}

/// Returns `true` when the cached key set can be used at `now`.
#[must_use]
pub const fn keys_fresh(now: u64, fetched_at: u64, max_age: u64) -> bool {
    fetched_at <= now && now - fetched_at < max_age
}

/// Returns `true` when an unknown `kid` can start a key refresh at `now`.
#[must_use]
pub const fn may_refresh_keys(now: u64, last_attempt: Option<u64>, min_interval: u64) -> bool {
    match last_attempt {
        Some(t) => now < t || now - t >= min_interval,
        None => true,
    }
}

/// Returns the wait in ms before retry number `attempt` (1-based):
/// `initial * 2^(attempt - 1)`, capped at `cap`.
#[must_use]
pub const fn backoff_ms(initial: u64, attempt: u32, cap: u64) -> u64 {
    let mut b = if initial <= cap { initial } else { cap };
    let mut i = 1;
    while i < attempt {
        // At the cap or at zero: stop early. A large `attempt` costs no time.
        if b == cap {
            return b;
        }
        if b == 0 {
            return b;
        }
        // Compare without overflow: `cap - b <= b` is `2b >= cap`.
        b = if cap - b <= b { cap } else { b * 2 };
        i += 1;
    }
    b
}

/// The class of one Connector attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// HTTP 2xx.
    Success,
    /// HTTP 401. The token is not accepted. The Connector did nothing.
    Unauthorized,
    /// HTTP 429.
    RateLimited,
    /// HTTP 5xx.
    ServerError,
    /// No HTTP reply: connect error or timeout.
    NetworkError,
    /// Other HTTP status.
    ClientError,
}

/// What the client does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Return the result as it is.
    Done,
    /// Wait this many ms, then try again.
    Retry(u64),
    /// Get a new token, then try again at once.
    RefreshToken,
    /// Stop. Return the last error.
    GiveUp,
}

/// Retry rules for one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryRule {
    /// Attempts in total, 1 to [`MAX_ATTEMPTS`].
    pub max_attempts: u32,
    /// First backoff for 5xx and network errors.
    pub initial_backoff_ms: u64,
    /// Largest wait before a retry.
    pub max_wait_ms: u64,
}

/// Decides the next step after attempt number `attempt` (1-based).
///
/// - 401: get a new token and retry at once, one time per call.
/// - 429: retry after `Retry-After` if attempts remain and the wait is not
///   more than the cap.
/// - 5xx and network errors: retry with backoff only when `idempotent`.
/// - Other outcomes: done.
#[must_use]
pub const fn decide(
    outcome: Outcome,
    attempt: u32,
    idempotent: bool,
    retry_after_ms: Option<u64>,
    token_refreshed: bool,
    rule: &RetryRule,
) -> Step {
    match outcome {
        Outcome::Success | Outcome::ClientError => Step::Done,
        Outcome::Unauthorized => {
            if !token_refreshed && attempt < rule.max_attempts {
                Step::RefreshToken
            } else {
                Step::GiveUp
            }
        }
        Outcome::RateLimited => {
            let wait = match retry_after_ms {
                Some(w) => w,
                None => DEFAULT_RETRY_AFTER_MS,
            };
            if attempt < rule.max_attempts && wait <= rule.max_wait_ms {
                Step::Retry(wait)
            } else {
                Step::GiveUp
            }
        }
        Outcome::ServerError | Outcome::NetworkError => {
            if idempotent && attempt < rule.max_attempts {
                Step::Retry(backoff_ms(
                    rule.initial_backoff_ms,
                    attempt,
                    rule.max_wait_ms,
                ))
            } else {
                Step::GiveUp
            }
        }
    }
}

/// Converts `Retry-After` seconds to ms. It saturates.
#[must_use]
pub const fn retry_after_ms(secs: u64) -> u64 {
    if secs <= u64::MAX / 1000 {
        secs * 1000
    } else {
        u64::MAX
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const RULE: RetryRule = RetryRule {
        max_attempts: 3,
        initial_backoff_ms: 100,
        max_wait_ms: 1_000,
    };

    #[test]
    fn verus_constants_match() {
        let spec = include_str!("../verus/policy.rs");
        for line in [
            "pub const DEFAULT_CLOCK_SKEW_SECS: u64 = 300;",
            "pub const MAX_KEY_AGE_SECS: u64 = 86_400;",
            "pub const TOKEN_REFRESH_MARGIN_SECS: u64 = 300;",
            "pub const MAX_ATTEMPTS: u32 = 10;",
            "pub const DEFAULT_RETRY_AFTER_MS: u64 = 1_000;",
        ] {
            assert!(spec.contains(line), "verus/policy.rs lacks {line}");
        }
    }

    /// The `{ ... }` block that starts at the last `start` before `anchor`,
    /// with no whitespace and no optional comma near `}` or `)`.
    fn block(src: &str, start: &str, anchor: &str) -> String {
        let at = src.find(anchor).unwrap_or_else(|| panic!("no {anchor}"));
        let from = src[..at + anchor.len()]
            .rfind(start)
            .unwrap_or_else(|| panic!("no {start}"));
        let open = from + src[from..].find('{').unwrap();
        let mut depth = 0;
        let mut end = open;
        for (i, c) in src[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                end = open + i;
                break;
            }
        }
        let flat: String = src[open..=end]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        flat.replace(",}", "}")
            .replace(",)", ")")
            .replace("},", "}")
    }

    /// The text from `anchor` to the end of its line, with no whitespace.
    fn line(src: &str, anchor: &str) -> String {
        let at = src.find(anchor).unwrap_or_else(|| panic!("no {anchor}"));
        src[at..]
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
    }

    #[test]
    fn verus_bodies_match() {
        let spec = include_str!("../verus/policy.rs");
        let code = include_str!("policy.rs");
        // Bodies with no proof code: the text must match.
        for (start, anchor) in [
            (
                "match outcome {",
                "Outcome::Success | Outcome::ClientError => Step::Done",
            ),
            (
                "if secs <= u64::MAX / 1000 {",
                "if secs <= u64::MAX / 1000 {",
            ),
            (
                "if expires_in <= u64::MAX - now {",
                "if expires_in <= u64::MAX - now {",
            ),
            ("let nbf_ok = match nbf {", "let nbf_ok = match nbf {"),
            (
                "match last_attempt {",
                "Some(t) => now < t || now - t >= min_interval,",
            ),
        ] {
            assert_eq!(
                block(spec, start, anchor),
                block(code, start, anchor),
                "{start}"
            );
        }
        for anchor in [
            "let exp_ok = ",
            "expires_at <= now || expires_at - now <= margin",
            "fetched_at <= now && now - fetched_at < max_age",
            "b = if cap - b <= b { cap } else { b * 2 };",
        ] {
            assert_eq!(line(spec, anchor), line(code, anchor), "{anchor}");
        }
    }

    #[test]
    fn time_window_edges() {
        // exp edge with skew 300.
        assert!(token_time_valid(1_300, None, 1_000, 300));
        assert!(!token_time_valid(1_301, None, 1_000, 300));
        // nbf edge.
        assert!(token_time_valid(700, Some(1_000), 2_000, 300));
        assert!(!token_time_valid(699, Some(1_000), 2_000, 300));
        // Zero skew: nbf <= now <= exp.
        assert!(token_time_valid(5, Some(5), 5, 0));
        assert!(!token_time_valid(6, Some(5), 5, 0));
        assert!(!token_time_valid(4, Some(5), 5, 0));
        // No overflow at the ends.
        assert!(token_time_valid(u64::MAX, None, u64::MAX, u64::MAX));
        assert!(token_time_valid(0, Some(u64::MAX), u64::MAX, u64::MAX));
        assert!(!token_time_valid(u64::MAX, None, 0, u64::MAX - 1));
    }

    #[test]
    fn refresh_edges() {
        assert!(!token_needs_refresh(0, 301, 300));
        assert!(token_needs_refresh(1, 301, 300));
        assert!(token_needs_refresh(500, 400, 300));
        assert!(token_needs_refresh(u64::MAX, u64::MAX, 0));
        assert!(!token_needs_refresh(0, u64::MAX, u64::MAX - 1));
        assert_eq!(token_expires_at(10, 3_600), 3_610);
        assert_eq!(token_expires_at(u64::MAX - 1, 5), u64::MAX);
    }

    #[test]
    fn key_cache_edges() {
        assert!(keys_fresh(100, 100, 1));
        assert!(!keys_fresh(101, 100, 1));
        // A key set from the future is stale.
        assert!(!keys_fresh(99, 100, 1_000));
        assert!(!keys_fresh(100, 100, 0));
        assert!(may_refresh_keys(0, None, 300));
        assert!(!may_refresh_keys(299, Some(0), 300));
        assert!(may_refresh_keys(300, Some(0), 300));
        // The clock went back: allow, so a bad clock cannot block keys.
        assert!(may_refresh_keys(5, Some(10), 300));
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff_ms(100, 0, 1_000), 100);
        assert_eq!(backoff_ms(100, 1, 1_000), 100);
        assert_eq!(backoff_ms(100, 2, 1_000), 200);
        assert_eq!(backoff_ms(100, 4, 1_000), 800);
        assert_eq!(backoff_ms(100, 5, 1_000), 1_000);
        assert_eq!(backoff_ms(5_000, 1, 1_000), 1_000);
        assert_eq!(backoff_ms(u64::MAX, 60, u64::MAX), u64::MAX);
        assert_eq!(backoff_ms(1, u32::MAX, 8), 8);
        assert_eq!(backoff_ms(0, u32::MAX, 8), 0);
    }

    #[test]
    fn decide_table() {
        use Outcome::*;
        let d = |o, a, idem, ra, refreshed| decide(o, a, idem, ra, refreshed, &RULE);
        assert_eq!(d(Success, 1, false, None, false), Step::Done);
        assert_eq!(d(ClientError, 1, true, None, false), Step::Done);
        assert_eq!(d(Unauthorized, 1, false, None, false), Step::RefreshToken);
        assert_eq!(d(Unauthorized, 2, false, None, true), Step::GiveUp);
        assert_eq!(d(Unauthorized, 3, false, None, false), Step::GiveUp);
        assert_eq!(d(RateLimited, 1, false, Some(500), false), Step::Retry(500));
        assert_eq!(d(RateLimited, 1, false, None, false), Step::Retry(1_000));
        assert_eq!(d(RateLimited, 1, false, Some(1_001), false), Step::GiveUp);
        assert_eq!(d(RateLimited, 3, false, Some(1), false), Step::GiveUp);
        assert_eq!(d(ServerError, 1, false, None, false), Step::GiveUp);
        assert_eq!(d(ServerError, 2, true, None, false), Step::Retry(200));
        assert_eq!(d(NetworkError, 1, true, None, false), Step::Retry(100));
        assert_eq!(d(NetworkError, 3, true, None, false), Step::GiveUp);
    }

    #[test]
    fn retry_after_saturates() {
        assert_eq!(retry_after_ms(3), 3_000);
        assert_eq!(retry_after_ms(u64::MAX), u64::MAX);
    }

    fn outcome() -> impl Strategy<Value = Outcome> {
        prop_oneof![
            Just(Outcome::Success),
            Just(Outcome::Unauthorized),
            Just(Outcome::RateLimited),
            Just(Outcome::ServerError),
            Just(Outcome::NetworkError),
            Just(Outcome::ClientError),
        ]
    }

    proptest! {
        // Same properties as the Verus `ensures` clauses.
        #[test]
        fn decide_invariants(
            o in outcome(),
            attempt in 0u32..20,
            idem: bool,
            refreshed: bool,
            ra in proptest::option::of(0u64..5_000),
            max_attempts in 1u32..=MAX_ATTEMPTS,
            init in 0u64..10_000,
            cap in 0u64..10_000,
        ) {
            let rule = RetryRule { max_attempts, initial_backoff_ms: init, max_wait_ms: cap };
            let step = decide(o, attempt, idem, ra, refreshed, &rule);
            let wait = ra.unwrap_or(DEFAULT_RETRY_AFTER_MS);
            if o == Outcome::RateLimited && attempt < max_attempts && wait <= cap {
                prop_assert_eq!(step, Step::Retry(wait));
            }
            if matches!(o, Outcome::ServerError | Outcome::NetworkError) && idem && attempt < max_attempts {
                prop_assert_eq!(step, Step::Retry(backoff_ms(init, attempt, cap)));
            }
            if o == Outcome::Unauthorized && !refreshed && attempt < max_attempts {
                prop_assert_eq!(step, Step::RefreshToken);
            }
            match step {
                Step::Retry(w) => {
                    prop_assert!(attempt < max_attempts);
                    prop_assert!(w <= cap);
                    prop_assert!(o == Outcome::RateLimited || idem);
                    prop_assert!(o != Outcome::Unauthorized);
                }
                Step::RefreshToken => {
                    prop_assert!(attempt < max_attempts);
                    prop_assert!(o == Outcome::Unauthorized && !refreshed);
                }
                Step::Done => prop_assert!(matches!(o, Outcome::Success | Outcome::ClientError)),
                Step::GiveUp => prop_assert!(!matches!(o, Outcome::Success | Outcome::ClientError)),
            }
        }

        /// Same value as the Verus `spec_time_valid`, in `i128`.
        #[test]
        fn time_valid_matches_spec(now: u64, nbf: Option<u64>, exp: u64, skew: u64) {
            let (n, e, s) = (i128::from(now), i128::from(exp), i128::from(skew));
            let want = n <= e + s && nbf.is_none_or(|b| i128::from(b) <= n + s);
            prop_assert_eq!(token_time_valid(now, nbf, exp, skew), want);
        }

        #[test]
        fn refresh_matches_spec(now: u64, at: u64, margin: u64) {
            let want = i128::from(now) + i128::from(margin) >= i128::from(at);
            prop_assert_eq!(token_needs_refresh(now, at, margin), want);
            let sum = u128::from(now) + u128::from(margin);
            prop_assert_eq!(u128::from(token_expires_at(now, margin)), sum.min(u128::from(u64::MAX)));
        }

        #[test]
        fn key_rules_match_spec(now: u64, t: u64, d: u64, last: Option<u64>) {
            let want = t <= now && i128::from(now) < i128::from(t) + i128::from(d);
            prop_assert_eq!(keys_fresh(now, t, d), want);
            let want = last.is_none_or(|l| now < l || i128::from(now) >= i128::from(l) + i128::from(d));
            prop_assert_eq!(may_refresh_keys(now, last, d), want);
        }

        /// Same value as the Verus `spec_backoff`, from an independent
        /// formula: `min(initial * 2^(attempt - 1), cap)`, in `u128`.
        #[test]
        fn backoff_matches_closed_form(init: u64, n in 0u32..200, cap: u64) {
            let exp = n.saturating_sub(1).min(64);
            let want = (u128::from(init) << exp).min(u128::from(cap));
            prop_assert_eq!(u128::from(backoff_ms(init, n, cap)), want);
        }
    }
}
