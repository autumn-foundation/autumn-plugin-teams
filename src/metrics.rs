//! Counters and gauges for `/actuator/prometheus`.
//!
//! Label values come from fixed sets in code. They never come from activity
//! data.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use autumn_web::actuator::{MetricFamily, MetricKind, MetricSample, MetricsSource};

type Pair = (&'static str, &'static str);

/// Plugin metrics. Also a [`MetricsSource`].
#[derive(Debug, Default)]
pub struct TeamsMetrics {
    requests: Mutex<BTreeMap<&'static str, u64>>,
    handlers: Mutex<BTreeMap<Pair, u64>>,
    late: AtomicU64,
    api: Mutex<BTreeMap<Pair, u64>>,
    retries: Mutex<BTreeMap<&'static str, u64>>,
    tokens: Mutex<BTreeMap<&'static str, u64>>,
    keys: Mutex<BTreeMap<&'static str, u64>>,
    in_flight: AtomicU64,
}

impl TeamsMetrics {
    /// Makes empty metrics.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn request(&self, outcome: &'static str) {
        add(&self.requests, outcome);
    }

    pub(crate) fn handler(&self, kind: &'static str, outcome: &'static str) {
        add(&self.handlers, (kind, outcome));
    }

    /// An invoke handler that missed the reply window.
    pub(crate) fn late_ack(&self) {
        self.late.fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn api_call(&self, operation: &'static str, outcome: &'static str) {
        add(&self.api, (operation, outcome));
    }

    pub(crate) fn api_retry(&self, operation: &'static str) {
        add(&self.retries, operation);
    }

    pub(crate) fn token_request(&self, outcome: &'static str) {
        add(&self.tokens, outcome);
    }

    pub(crate) fn key_refresh(&self, outcome: &'static str) {
        add(&self.keys, outcome);
    }

    pub(crate) fn in_flight_add(&self, delta: i64) {
        let step = delta.unsigned_abs();
        // Saturate at 0: an extra decrement must not wrap below 0.
        let _ = self
            .in_flight
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
                Some(if delta >= 0 {
                    v.saturating_add(step)
                } else {
                    v.saturating_sub(step)
                })
            });
    }

    /// Inbound requests by outcome, for example `ok` or `bad_signature`.
    #[must_use]
    pub fn requests(&self, outcome: &str) -> u64 {
        get(&self.requests, |k| *k == outcome)
    }

    /// Handler runs by kind (`message`, `command`, `invoke`, ...) and outcome
    /// (`ok`, `error`, `panic`).
    #[must_use]
    pub fn handler_runs(&self, kind: &str, outcome: &str) -> u64 {
        get(&self.handlers, |(k, o)| *k == kind && *o == outcome)
    }

    /// Invoke handlers that missed the reply window.
    #[must_use]
    pub fn late_acks(&self) -> u64 {
        self.late.load(Ordering::SeqCst)
    }

    /// Connector calls by operation and outcome.
    #[must_use]
    pub fn api_calls(&self, operation: &str, outcome: &str) -> u64 {
        get(&self.api, |(k, o)| *k == operation && *o == outcome)
    }

    /// Connector retries by operation.
    #[must_use]
    pub fn api_retries(&self, operation: &str) -> u64 {
        get(&self.retries, |k| *k == operation)
    }

    /// Token requests by outcome.
    #[must_use]
    pub fn token_requests(&self, outcome: &str) -> u64 {
        get(&self.tokens, |k| *k == outcome)
    }

    /// Key set loads by outcome.
    #[must_use]
    pub fn key_refreshes(&self, outcome: &str) -> u64 {
        get(&self.keys, |k| *k == outcome)
    }

    /// Handlers that run now.
    #[must_use]
    pub fn in_flight(&self) -> u64 {
        self.in_flight.load(Ordering::SeqCst)
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn add<K: Ord>(m: &Mutex<BTreeMap<K, u64>>, key: K) {
    *lock(m).entry(key).or_default() += 1;
}

fn get<K>(m: &Mutex<BTreeMap<K, u64>>, f: impl Fn(&K) -> bool) -> u64 {
    lock(m).iter().find(|(k, _)| f(k)).map_or(0, |(_, n)| *n)
}

#[allow(clippy::cast_precision_loss)] // Counters stay far below 2^53.
const fn as_f64(v: u64) -> f64 {
    v as f64
}

fn sample(pairs: &[(&str, &str)], value: u64) -> MetricSample {
    MetricSample {
        labels: pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        value: as_f64(value),
    }
}

fn family(name: &str, help: &str, kind: MetricKind, samples: Vec<MetricSample>) -> MetricFamily {
    MetricFamily {
        name: name.to_owned(),
        help: help.to_owned(),
        kind,
        samples,
    }
}

fn single(m: &Mutex<BTreeMap<&'static str, u64>>, label: &str) -> Vec<MetricSample> {
    lock(m)
        .iter()
        .map(|(v, n)| sample(&[(label, v)], *n))
        .collect()
}

fn pairs(m: &Mutex<BTreeMap<Pair, u64>>, a: &str) -> Vec<MetricSample> {
    lock(m)
        .iter()
        .map(|((x, y), n)| sample(&[(a, x), ("outcome", y)], *n))
        .collect()
}

impl MetricsSource for TeamsMetrics {
    fn collect(&self) -> Vec<MetricFamily> {
        use MetricKind::{Counter, Gauge};
        vec![
            family(
                "teams_requests_total",
                "Inbound Teams requests by outcome.",
                Counter,
                single(&self.requests, "outcome"),
            ),
            family(
                "teams_handler_runs_total",
                "Handler runs by kind and outcome.",
                Counter,
                pairs(&self.handlers, "kind"),
            ),
            family(
                "teams_late_acks_total",
                "Invoke handlers that missed the reply window.",
                Counter,
                vec![sample(&[], self.late_acks())],
            ),
            family(
                "teams_api_calls_total",
                "Connector calls by operation and outcome.",
                Counter,
                pairs(&self.api, "operation"),
            ),
            family(
                "teams_api_retries_total",
                "Connector retries by operation.",
                Counter,
                single(&self.retries, "operation"),
            ),
            family(
                "teams_token_requests_total",
                "Outbound token requests by outcome.",
                Counter,
                single(&self.tokens, "outcome"),
            ),
            family(
                "teams_key_refreshes_total",
                "Inbound key set loads by outcome.",
                Counter,
                single(&self.keys, "outcome"),
            ),
            family(
                "teams_handlers_in_flight",
                "Handlers that run now.",
                Gauge,
                vec![sample(&[], self.in_flight())],
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_families() {
        let m = TeamsMetrics::new();
        m.request("ok");
        m.request("ok");
        m.handler("command", "ok");
        m.late_ack();
        m.api_call("send_to_conversation", "ok");
        m.api_retry("get_members");
        m.token_request("ok");
        m.key_refresh("error");
        m.in_flight_add(2);
        m.in_flight_add(-1);
        assert_eq!(m.requests("ok"), 2);
        assert_eq!(m.requests("bad_signature"), 0);
        assert_eq!(m.handler_runs("command", "ok"), 1);
        assert_eq!(m.handler_runs("command", "error"), 0);
        assert_eq!(m.late_acks(), 1);
        assert_eq!(m.api_calls("send_to_conversation", "ok"), 1);
        assert_eq!(m.api_retries("get_members"), 1);
        assert_eq!(m.token_requests("ok"), 1);
        assert_eq!(m.key_refreshes("error"), 1);
        assert_eq!(m.in_flight(), 1);
        let f = m.collect();
        assert_eq!(f.len(), 8);
        let req = f.iter().find(|f| f.name == "teams_requests_total").unwrap();
        assert_eq!(req.kind, MetricKind::Counter);
        assert!((req.samples[0].value - 2.0).abs() < f64::EPSILON);
        assert_eq!(
            req.samples[0].labels,
            vec![("outcome".to_owned(), "ok".to_owned())]
        );
        let h = f
            .iter()
            .find(|f| f.name == "teams_handler_runs_total")
            .unwrap();
        assert_eq!(
            h.samples[0].labels,
            vec![
                ("kind".to_owned(), "command".to_owned()),
                ("outcome".to_owned(), "ok".to_owned())
            ]
        );
        let g = f
            .iter()
            .find(|f| f.name == "teams_handlers_in_flight")
            .unwrap();
        assert_eq!(g.kind, MetricKind::Gauge);
        assert!((g.samples[0].value - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn in_flight_never_wraps_below_zero() {
        let m = TeamsMetrics::new();
        m.in_flight_add(-1);
        assert_eq!(m.in_flight(), 0);
    }
}
