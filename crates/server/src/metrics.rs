//! What an operator watches (M5 D5): `GET /metrics` on the health port, a JSON snapshot; `GET /metrics/prometheus`
//! the same in Prometheus' text format (the `prometheus` feature). Counts only, never a Capcom ID. Counters live here
//! (atomics, bumped where things happen); gauges are read from the world when asked. No history: Prometheus keeps it.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;

use crate::Shared;
use crate::game::world::SERVERS;

/// Request latency buckets, seconds (upper bounds; the last one counts everything).
const BUCKETS: [f64; 7] = [0.0005, 0.001, 0.005, 0.01, 0.05, 0.1, f64::INFINITY];

#[derive(Default)]
pub struct Metrics {
    requests: AtomicU64,
    request_micros: AtomicU64,
    buckets: [AtomicU64; BUCKETS.len()],
    pub pairs_direct: AtomicU64,
    pub pairs_relayed: AtomicU64,
    pub reports_success: AtomicU64,
    pub reports_failure: AtomicU64,
    pub relay_pairs_open: AtomicU64,
    pub relay_bytes: AtomicU64,
}

impl Metrics {
    /// A request handled (any role), and how long it took.
    pub fn request(&self, took: Duration) {
        self.requests.fetch_add(1, Ordering::Relaxed);
        let micros = u64::try_from(took.as_micros()).unwrap_or(u64::MAX);
        self.request_micros.fetch_add(micros, Ordering::Relaxed);
        let seconds = took.as_secs_f64();
        for (bucket, &bound) in self.buckets.iter().zip(&BUCKETS) {
            if seconds <= bound {
                bucket.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

pub fn add(counter: &AtomicU64, n: u64) {
    counter.fetch_add(n, Ordering::Relaxed);
}

#[derive(Serialize)]
pub struct Snapshot {
    /// Logged in on FMP.
    pub players: usize,
    /// Per world server (by name), the players there.
    pub world_servers: BTreeMap<&'static str, usize>,
    pub cities: usize,
    pub quests: usize,
    pub natneg: Natneg,
    pub relay: Relay,
    pub requests: Requests,
}

#[derive(Serialize)]
pub struct Natneg {
    pub pairs_direct: u64,
    pub pairs_relayed: u64,
    pub reports_success: u64,
    pub reports_failure: u64,
}

#[derive(Serialize)]
pub struct Relay {
    pub pairs_open: u64,
    pub bytes: u64,
}

#[derive(Serialize)]
pub struct Requests {
    pub count: u64,
    pub seconds: f64,
    /// Requests that took at most this many seconds (cumulative), by upper bound.
    pub buckets: Vec<(String, u64)>,
}

pub fn snapshot(shared: &Shared) -> Snapshot {
    let m = &shared.metrics;
    let get = |c: &AtomicU64| c.load(Ordering::Relaxed);
    let (by_server, cities, quests) = shared.world.lock().unwrap().census();
    let world_servers = SERVERS
        .iter()
        .enumerate()
        .map(|(i, (name, _))| (*name, by_server.get(&(i as u32 + 1)).copied().unwrap_or(0)))
        .collect();
    Snapshot {
        players: shared.players.count(),
        world_servers,
        cities,
        quests,
        natneg: Natneg {
            pairs_direct: get(&m.pairs_direct),
            pairs_relayed: get(&m.pairs_relayed),
            reports_success: get(&m.reports_success),
            reports_failure: get(&m.reports_failure),
        },
        relay: Relay {
            pairs_open: get(&m.relay_pairs_open),
            bytes: get(&m.relay_bytes),
        },
        requests: Requests {
            count: get(&m.requests),
            seconds: get(&m.request_micros) as f64 / 1e6,
            buckets: BUCKETS
                .iter()
                .zip(&m.buckets)
                .map(|(bound, n)| (bound_label(*bound), get(n)))
                .collect(),
        },
    }
}

fn bound_label(bound: f64) -> String {
    match bound.is_infinite() {
        true => "+Inf".to_owned(),
        false => bound.to_string(),
    }
}

/// Prometheus' text exposition format.
#[cfg(feature = "prometheus")]
pub fn prometheus(s: &Snapshot) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let mut metric = |name: &str, kind: &str, help: &str, samples: &[(String, f64)]| {
        let _ = writeln!(
            out,
            "# HELP mh3sp_{name} {help}\n# TYPE mh3sp_{name} {kind}"
        );
        for (labels, value) in samples {
            let _ = writeln!(out, "mh3sp_{name}{labels} {value}");
        }
    };
    let one = |v: f64| vec![(String::new(), v)];
    metric(
        "players",
        "gauge",
        "Players logged in.",
        &one(s.players as f64),
    );
    let servers: Vec<_> = s
        .world_servers
        .iter()
        .map(|(name, n)| (format!("{{world_server=\"{name}\"}}"), *n as f64))
        .collect();
    metric(
        "world_server_players",
        "gauge",
        "Players per world server.",
        &servers,
    );
    metric("cities", "gauge", "Cities open.", &one(s.cities as f64));
    metric("quests", "gauge", "Quests open.", &one(s.quests as f64));
    let n = &s.natneg;
    let pairs = vec![
        ("{link=\"direct\"}".to_owned(), n.pairs_direct as f64),
        ("{link=\"relay\"}".to_owned(), n.pairs_relayed as f64),
    ];
    metric(
        "natneg_pairs_total",
        "counter",
        "NAT negotiations paired.",
        &pairs,
    );
    let reports = vec![
        ("{result=\"success\"}".to_owned(), n.reports_success as f64),
        ("{result=\"failure\"}".to_owned(), n.reports_failure as f64),
    ];
    metric(
        "natneg_reports_total",
        "counter",
        "NAT negotiation reports.",
        &reports,
    );
    metric(
        "relay_pairs",
        "gauge",
        "Relayed pairs open.",
        &one(s.relay.pairs_open as f64),
    );
    metric(
        "relay_bytes_total",
        "counter",
        "Bytes relayed.",
        &one(s.relay.bytes as f64),
    );
    let r = &s.requests;
    let mut histogram: Vec<_> = r
        .buckets
        .iter()
        .map(|(bound, n)| (format!("_bucket{{le=\"{bound}\"}}"), *n as f64))
        .collect();
    histogram.push(("_sum".to_owned(), r.seconds));
    histogram.push(("_count".to_owned(), r.count as f64));
    metric(
        "request_seconds",
        "histogram",
        "Time to handle a request.",
        &histogram,
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latencies_fill_cumulative_buckets() {
        let m = Metrics::default();
        m.request(Duration::from_micros(300));
        m.request(Duration::from_millis(20));
        let counts: Vec<u64> = m
            .buckets
            .iter()
            .map(|b| b.load(Ordering::Relaxed))
            .collect();
        assert_eq!(counts, [1, 1, 1, 1, 2, 2, 2]);
        assert_eq!(m.requests.load(Ordering::Relaxed), 2);
    }

    #[cfg(feature = "prometheus")]
    #[test]
    fn prometheus_text() {
        let m = Metrics::default();
        m.request(Duration::from_millis(2));
        let s = Snapshot {
            players: 3,
            world_servers: [("Valor1", 3)].into(),
            cities: 1,
            quests: 0,
            natneg: Natneg {
                pairs_direct: 2,
                pairs_relayed: 1,
                reports_success: 3,
                reports_failure: 1,
            },
            relay: Relay {
                pairs_open: 1,
                bytes: 640,
            },
            requests: Requests {
                count: 1,
                seconds: 0.002,
                buckets: vec![("0.001".into(), 0), ("+Inf".into(), 1)],
            },
        };
        let text = prometheus(&s);
        for line in [
            "# TYPE mh3sp_players gauge",
            "mh3sp_players 3",
            "mh3sp_world_server_players{world_server=\"Valor1\"} 3",
            "mh3sp_natneg_pairs_total{link=\"relay\"} 1",
            "mh3sp_request_seconds_bucket{le=\"+Inf\"} 1",
            "mh3sp_request_seconds_count 1",
        ] {
            assert!(text.lines().any(|l| l == line), "{line} missing:\n{text}");
        }
    }
}
