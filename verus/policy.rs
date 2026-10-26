//! Verus spec and proof for `src/policy.rs`.
//!
//! Keep this file in step with `src/policy.rs`.
//! Run: `verus verus/policy.rs`.

use vstd::prelude::*;

verus! {

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

// --------------------------------------------------------- token time window

/// Spec: `exp` and `nbf` are in the window, with `skew` on each side.
pub open spec fn spec_time_valid(now: int, nbf: Option<u64>, exp: int, skew: int) -> bool {
    now <= exp + skew && match nbf {
        Some(n) => n as int <= now + skew,
        None => true,
    }
}

/// Returns `true` when a token with `exp` and `nbf` is valid at `now`.
pub fn token_time_valid(now: u64, nbf: Option<u64>, exp: u64, skew: u64) -> (r: bool)
    ensures
        r == spec_time_valid(now as int, nbf, exp as int, skew as int),
{
    // Compare without overflow: `now - exp <= skew` only when `now > exp`.
    let exp_ok = now <= exp || now - exp <= skew;
    let nbf_ok = match nbf {
        Some(n) => n <= now || n - now <= skew,
        None => true,
    };
    exp_ok && nbf_ok
}

/// A token after `exp + skew` is never valid.
proof fn lemma_expired_rejected(now: int, nbf: Option<u64>, exp: int, skew: int)
    requires
        now > exp + skew,
    ensures
        !spec_time_valid(now, nbf, exp, skew),
{
}

/// A token before `nbf - skew` is never valid.
proof fn lemma_early_rejected(now: int, n: u64, exp: int, skew: int)
    requires
        now + skew < n as int,
    ensures
        !spec_time_valid(now, Some(n), exp, skew),
{
}

/// A larger skew accepts all that a smaller skew accepts.
proof fn lemma_skew_monotonic(now: int, nbf: Option<u64>, exp: int, s1: int, s2: int)
    requires
        s1 <= s2,
        spec_time_valid(now, nbf, exp, s1),
    ensures
        spec_time_valid(now, nbf, exp, s2),
{
}

/// With zero skew, a valid token has `nbf <= now <= exp`.
proof fn lemma_zero_skew(now: int, n: u64, exp: int)
    requires
        spec_time_valid(now, Some(n), exp, 0),
    ensures
        n as int <= now <= exp,
{
}

// -------------------------------------------------------- outbound token cache

/// Spec: the token is in its last `margin` seconds, or expired.
pub open spec fn spec_needs_refresh(now: int, expires_at: int, margin: int) -> bool {
    now + margin >= expires_at
}

/// Returns `true` when the cached token must be replaced at `now`.
pub fn token_needs_refresh(now: u64, expires_at: u64, margin: u64) -> (r: bool)
    ensures
        r == spec_needs_refresh(now as int, expires_at as int, margin as int),
{
    expires_at <= now || expires_at - now <= margin
}

/// A token that does not need a refresh is valid for more than `margin`
/// seconds.
proof fn lemma_kept_token_is_live(now: int, expires_at: int, margin: int)
    requires
        margin >= 0,
        !spec_needs_refresh(now, expires_at, margin),
    ensures
        now + margin < expires_at,
        now < expires_at,
{
}

/// Returns `now + expires_in`. It saturates.
pub fn token_expires_at(now: u64, expires_in: u64) -> (r: u64)
    ensures
        now as int + expires_in as int <= u64::MAX as int ==> r == now + expires_in,
        now as int + expires_in as int > u64::MAX as int ==> r == u64::MAX,
{
    if expires_in <= u64::MAX - now {
        now + expires_in
    } else {
        u64::MAX
    }
}

// ------------------------------------------------------------- inbound keys

/// Spec: the key set is younger than `max_age`. A key set from the future
/// (the clock went back) is not fresh.
pub open spec fn spec_keys_fresh(now: int, fetched_at: int, max_age: int) -> bool {
    fetched_at <= now && now < fetched_at + max_age
}

/// Returns `true` when the cached key set can be used at `now`.
pub fn keys_fresh(now: u64, fetched_at: u64, max_age: u64) -> (r: bool)
    ensures
        r == spec_keys_fresh(now as int, fetched_at as int, max_age as int),
{
    fetched_at <= now && now - fetched_at < max_age
}

/// Fresh keys with `max_age <= MAX_KEY_AGE_SECS` are less than 24 h old.
proof fn lemma_fresh_keys_are_young(now: int, fetched_at: int, max_age: int)
    requires
        max_age <= MAX_KEY_AGE_SECS as int,
        spec_keys_fresh(now, fetched_at, max_age),
    ensures
        now - fetched_at < MAX_KEY_AGE_SECS as int,
{
}

/// Spec: a refresh for an unknown key is allowed.
pub open spec fn spec_may_refresh(now: int, last_attempt: Option<u64>, min_interval: int) -> bool {
    match last_attempt {
        Some(t) => now < t as int || now >= t as int + min_interval,
        None => true,
    }
}

/// Returns `true` when an unknown `kid` can start a key refresh at `now`.
pub fn may_refresh_keys(now: u64, last_attempt: Option<u64>, min_interval: u64) -> (r: bool)
    ensures
        r == spec_may_refresh(now as int, last_attempt, min_interval as int),
{
    match last_attempt {
        Some(t) => now < t || now - t >= min_interval,
        None => true,
    }
}

/// With a clock that does not go back, two allowed refreshes are
/// `min_interval` or more apart. Unknown `kid` values cannot flood the key
/// endpoint.
proof fn lemma_refresh_rate_bounded(t1: u64, t2: int, min_interval: int)
    requires
        t1 as int <= t2,
        spec_may_refresh(t2, Some(t1), min_interval),
    ensures
        t2 - t1 as int >= min_interval,
{
}

// ------------------------------------------------------------------ backoff

pub open spec fn spec_min(a: int, b: int) -> int {
    if a <= b { a } else { b }
}

/// Spec: `initial * 2^(attempt - 1)`, capped at `cap`.
pub open spec fn spec_backoff(initial: int, attempt: int, cap: int) -> int
    decreases attempt,
{
    if attempt <= 1 {
        spec_min(initial, cap)
    } else {
        spec_min(spec_backoff(initial, attempt - 1, cap) * 2, cap)
    }
}

proof fn lemma_backoff_bounds(initial: int, attempt: int, cap: int)
    requires
        initial >= 0,
        cap >= 0,
    ensures
        0 <= spec_backoff(initial, attempt, cap) <= cap,
    decreases attempt,
{
    if attempt > 1 {
        lemma_backoff_bounds(initial, attempt - 1, cap);
    }
}

/// The backoff does not decrease from one attempt to the next.
proof fn lemma_backoff_monotonic(initial: int, attempt: int, cap: int)
    requires
        initial >= 0,
        cap >= 0,
        attempt >= 1,
    ensures
        spec_backoff(initial, attempt, cap) <= spec_backoff(initial, attempt + 1, cap),
{
    lemma_backoff_bounds(initial, attempt, cap);
}

/// Once the backoff reaches the cap, it stays at the cap.
proof fn lemma_backoff_stays_at_cap(initial: int, i: int, j: int, cap: int)
    requires
        cap >= 0,
        1 <= i <= j,
        spec_backoff(initial, i, cap) == cap,
    ensures
        spec_backoff(initial, j, cap) == cap,
    decreases j - i,
{
    if j > i {
        lemma_backoff_stays_at_cap(initial, i, j - 1, cap);
    }
}

/// Once the backoff is zero, it stays zero.
proof fn lemma_backoff_stays_at_zero(initial: int, i: int, j: int, cap: int)
    requires
        cap >= 0,
        1 <= i <= j,
        spec_backoff(initial, i, cap) == 0,
    ensures
        spec_backoff(initial, j, cap) == 0,
    decreases j - i,
{
    if j > i {
        lemma_backoff_stays_at_zero(initial, i, j - 1, cap);
    }
}

/// Returns the wait in ms before retry number `attempt` (1-based).
pub fn backoff_ms(initial: u64, attempt: u32, cap: u64) -> (r: u64)
    ensures
        r <= cap,
        r == spec_backoff(initial as int, attempt as int, cap as int),
{
    let mut b: u64 = if initial <= cap { initial } else { cap };
    let mut i: u32 = 1;
    while i < attempt
        invariant
            1 <= i,
            attempt == 0 ==> i == 1,
            attempt >= 1 ==> i <= attempt,
            b <= cap,
            b == spec_backoff(initial as int, i as int, cap as int),
        decreases attempt - i,
    {
        // At the cap or at zero: stop early. A large `attempt` costs no time.
        if b == cap {
            proof {
                lemma_backoff_stays_at_cap(initial as int, i as int, attempt as int, cap as int);
            }
            return b;
        }
        if b == 0 {
            proof {
                lemma_backoff_stays_at_zero(initial as int, i as int, attempt as int, cap as int);
            }
            return b;
        }
        // Compare without overflow: `cap - b <= b` is `2b >= cap`.
        b = if cap - b <= b { cap } else { b * 2 };
        i = i + 1;
    }
    // Attempt 0 and attempt 1 have the same spec value.
    assert(attempt == 0 ==> spec_backoff(initial as int, attempt as int, cap as int)
        == spec_backoff(initial as int, 1, cap as int));
    b
}

// ------------------------------------------------------------ retry decision

/// The class of one Connector attempt.
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
pub struct RetryRule {
    /// Attempts in total, 1 to `MAX_ATTEMPTS`.
    pub max_attempts: u32,
    /// First backoff for 5xx and network errors.
    pub initial_backoff_ms: u64,
    /// Largest wait before a retry.
    pub max_wait_ms: u64,
}

/// Spec: a wait-and-retry is allowed for this outcome.
pub open spec fn spec_retryable(outcome: Outcome, idempotent: bool) -> bool {
    match outcome {
        Outcome::RateLimited => true,
        Outcome::ServerError | Outcome::NetworkError => idempotent,
        _ => false,
    }
}

/// Decides the next step after attempt number `attempt` (1-based).
/// `token_refreshed` is `true` after the call got a new token one time.
pub fn decide(
    outcome: Outcome,
    attempt: u32,
    idempotent: bool,
    retry_after_ms: Option<u64>,
    token_refreshed: bool,
    rule: &RetryRule,
) -> (s: Step)
    ensures
        // Bounded attempts.
        (s is Retry || s is RefreshToken) ==> attempt < rule.max_attempts,
        // Bounded wait.
        s is Retry ==> s->Retry_0 <= rule.max_wait_ms,
        // A send never repeats after a 5xx or a network error.
        s is Retry ==> spec_retryable(outcome, idempotent),
        // A token refresh happens only for a 401, and only one time.
        s is RefreshToken ==> (outcome is Unauthorized && !token_refreshed),
        // Success and client errors end the call. Only they give Done.
        (outcome is Success || outcome is ClientError) ==> s is Done,
        s is Done ==> (outcome is Success || outcome is ClientError),
        // The first 401 with attempts left gets a new token.
        (outcome is Unauthorized && !token_refreshed && attempt < rule.max_attempts)
            ==> s is RefreshToken,
        // An idempotent call with attempts left retries a 5xx or network
        // error after exactly the capped backoff.
        ((outcome is ServerError || outcome is NetworkError) && idempotent
            && attempt < rule.max_attempts) ==> s == Step::Retry(spec_backoff(
            rule.initial_backoff_ms as int,
            attempt as int,
            rule.max_wait_ms as int,
        ) as u64),
        // A 429 with a wait in the cap and attempts left always retries.
        (outcome is RateLimited && attempt < rule.max_attempts && (match retry_after_ms {
            Some(w) => w <= rule.max_wait_ms,
            None => DEFAULT_RETRY_AFTER_MS <= rule.max_wait_ms,
        })) ==> s is Retry,
        // A 429 retry waits exactly as long as the Connector asks.
        (outcome is RateLimited && s is Retry) ==> s->Retry_0 == (match retry_after_ms {
            Some(w) => w,
            None => DEFAULT_RETRY_AFTER_MS,
        }),
{
    match outcome {
        Outcome::Success | Outcome::ClientError => Step::Done,
        Outcome::Unauthorized => {
            if !token_refreshed && attempt < rule.max_attempts {
                Step::RefreshToken
            } else {
                Step::GiveUp
            }
        },
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
        },
        Outcome::ServerError | Outcome::NetworkError => {
            if idempotent && attempt < rule.max_attempts {
                Step::Retry(backoff_ms(rule.initial_backoff_ms, attempt, rule.max_wait_ms))
            } else {
                Step::GiveUp
            }
        },
    }
}

/// Converts `Retry-After` seconds to ms. It saturates and does not overflow.
pub fn retry_after_ms(secs: u64) -> (r: u64)
    ensures
        secs <= u64::MAX / 1000 ==> r == secs * 1000,
        secs > u64::MAX / 1000 ==> r == u64::MAX,
{
    if secs <= u64::MAX / 1000 {
        secs * 1000
    } else {
        u64::MAX
    }
}

/// One step of the client loop: a retry at `attempt < max` gives a next
/// attempt that is still `<= max`. The loop itself (start at 1, add 1 per
/// retry, set `token_refreshed` after a refresh) is in `src/client.rs`.
/// Tests check it.
proof fn lemma_attempts_bounded(attempt: u32, max_attempts: u32)
    requires
        attempt < max_attempts,
    ensures
        attempt + 1 <= max_attempts,
{
}

fn main() {
}

} // verus!
