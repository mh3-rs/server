//! The relay's address when `external_address` is a domain: what is known of it, when it must be looked up again, and
//! the lookup. In memory only: a restart looks it up at its start. [`Address`] holds the rules and takes `now` as an
//! argument, so they are tested without waiting.
//!
//! The lookup starts at the root servers and follows the referrals to the domain's own nameservers (a recursor,
//! `hickory-resolver`), never the system's resolver: on a home network it answers a split horizon's private address
//! (the homelab's `pband.ch` is `192.168.1.50`), which no console could reach (ADR 0019). Nor a public resolver:
//! a third party, and not the zone's own answer.
//!
//! - An answer is used for [`FRESH`]: our cache is the only one (the recursor is built anew for each lookup, since
//!   its own cache would keep a record for its TTL, a day at times, and hide an IP that changed).
//! - A failed lookup keeps the last good address, for as long as lookups fail (no maximum age: during an outage it
//!   is the best guess there is), and none is tried for [`RETRY`]: a DNS outage must not stall NAT negotiation on every
//!   relayed pair.
//! - With no address known, the caller connects the pair directly.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::proto::op::Query;
use hickory_resolver::proto::rr::{Name, RData, RecordType};
use hickory_resolver::recursor::{Recursor, RecursorOptions};
use tracing::{info, warn};

/// How long a looked up address is used.
pub(crate) const FRESH: Duration = Duration::from_secs(5 * 60);
/// How long after a failed lookup the next one waits.
pub(crate) const RETRY: Duration = Duration::from_secs(30);
/// A lookup that takes longer fails, and NAT negotiation waits for it. The recursor asks one nameserver at a time and
/// gives each 5 s, and a recursor built anew knows none to be slow: with a dead one among a domain's, the lookup needs
/// more than 5 s to reach a live one.
const LOOKUP_CAP: Duration = Duration::from_secs(8);

/// The root servers' IPv4 addresses, a to m (<https://www.iana.org/domains/root/servers>): all 13 answered `. NS` on
/// 2026-11-27.
const ROOTS: [Ipv4Addr; 13] = [
    Ipv4Addr::new(198, 41, 0, 4),
    Ipv4Addr::new(170, 247, 170, 2),
    Ipv4Addr::new(192, 33, 4, 12),
    Ipv4Addr::new(199, 7, 91, 13),
    Ipv4Addr::new(192, 203, 230, 10),
    Ipv4Addr::new(192, 5, 5, 241),
    Ipv4Addr::new(192, 112, 36, 4),
    Ipv4Addr::new(198, 97, 190, 53),
    Ipv4Addr::new(192, 36, 148, 17),
    Ipv4Addr::new(192, 58, 128, 30),
    Ipv4Addr::new(193, 0, 14, 129),
    Ipv4Addr::new(199, 7, 83, 42),
    Ipv4Addr::new(202, 12, 27, 33),
];

/// What the caller does next.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Use this address (`None`: none is known, connect directly); no lookup now.
    Use(Option<Ipv4Addr>),
    /// Look the name up, then say how it went ([`Address::found`], [`Address::failed`]).
    Lookup,
}

#[derive(Debug, Default)]
pub(crate) struct Address {
    /// The last good answer and when it was looked up.
    known: Option<(Ipv4Addr, Instant)>,
    /// When the last lookup failed. A success needn't clear it: the next stale step is 5 minutes on, past the wait.
    failed: Option<Instant>,
}

impl Address {
    pub fn step(&self, now: Instant) -> Step {
        if let Some((ip, at)) = self.known
            && now.saturating_duration_since(at) < FRESH
        {
            return Step::Use(Some(ip));
        }
        match self.failed {
            Some(at) if now.saturating_duration_since(at) < RETRY => {
                Step::Use(self.known.map(|(ip, _)| ip))
            }
            _ => Step::Lookup,
        }
    }

    /// A lookup answered `ip`. Returns the address it replaces (`None` for the first one).
    pub fn found(&mut self, ip: Ipv4Addr, now: Instant) -> Option<Ipv4Addr> {
        self.known.replace((ip, now)).map(|(before, _)| before)
    }

    /// A lookup failed: the last good address, if any, stays in use.
    pub fn failed(&mut self, now: Instant) -> Option<Ipv4Addr> {
        self.failed = Some(now);
        self.known.map(|(ip, _)| ip)
    }
}

/// A domain and what is known of its address.
pub struct Domain {
    name: String,
    /// Tests only (`Config::test_roots`): name servers standing in for the root servers.
    test_roots: Option<Vec<IpAddr>>,
    state: Mutex<Address>,
}

impl Domain {
    pub fn new(name: String, test_roots: Option<Vec<IpAddr>>) -> Self {
        Domain {
            name,
            test_roots,
            state: Mutex::default(),
        }
    }

    /// The address to hand out: the one known, after a lookup when it is due; `None` when none is known (the caller
    /// connects directly). Logs what an operator needs to see: the address when it is first found or changes, and
    /// each failed lookup.
    pub async fn ip(&self) -> Option<Ipv4Addr> {
        self.ip_at(Instant::now()).await
    }

    /// [`Domain::ip`] at the instant `now`: a test moves the clock to see the 5 minutes and the 30 seconds go by
    /// without waiting for them.
    pub async fn ip_at(&self, now: Instant) -> Option<Ipv4Addr> {
        let step = self.state.lock().unwrap().step(now);
        if let Step::Use(ip) = step {
            return ip;
        }
        let found = lookup(&self.name, self.test_roots.as_deref()).await;
        let mut state = self.state.lock().unwrap();
        match found {
            Ok(ip) => {
                match state.found(ip, now) {
                    Some(before) if before == ip => {}
                    Some(before) => {
                        info!(domain = %self.name, %ip, %before, "external address changed")
                    }
                    None => info!(domain = %self.name, %ip, "external address resolved"),
                }
                Some(ip)
            }
            Err(error) => {
                let kept = state.failed(now);
                match kept {
                    Some(ip) => warn!(
                        domain = %self.name, %error, %ip,
                        "external address lookup failed: the last address is kept"
                    ),
                    None => warn!(
                        domain = %self.name, %error,
                        "external address lookup failed: no address known, relayed pairs connect directly"
                    ),
                }
                kept
            }
        }
    }
}

/// `name`'s first A record (CNAMEs are followed by the recursor), looked up from the root servers or, in tests,
/// from `test_roots`, in which case the recursor may query loopback servers (it refuses them otherwise). A fresh
/// recursor each time: its cache would keep a record for its TTL (see the module).
async fn lookup(name: &str, test_roots: Option<&[IpAddr]>) -> Result<Ipv4Addr, String> {
    let mut options = RecursorOptions::default();
    let roots: Vec<IpAddr> = match test_roots {
        Some(roots) => {
            options.deny_server.clear();
            roots.to_vec()
        }
        None => ROOTS.map(IpAddr::V4).to_vec(),
    };
    let recursor = Recursor::with_options(&roots, options, TokioRuntimeProvider::default())
        .map_err(|e| e.to_string())?;
    let name = Name::from_ascii(format!("{name}.")).map_err(|e| e.to_string())?;
    let query = Query::query(name, RecordType::A);
    let answer = tokio::time::timeout(LOOKUP_CAP, recursor.resolve(query, Instant::now(), false))
        .await
        .map_err(|_| format!("no answer in {} s", LOOKUP_CAP.as_secs()))?
        .map_err(|e| e.to_string())?;
    let mut first = None;
    for record in &answer.answers {
        if let RData::A(a) = &record.data {
            if public(a.0) {
                return Ok(a.0);
            }
            first.get_or_insert(a.0);
        }
    }
    Err(match first {
        Some(ip) => format!("{ip} is not a public address"),
        None => "no A record".to_owned(),
    })
}

/// An address consoles on the internet can be sent to. A dynamic DNS client that publishes the LAN address, or a zone
/// with `127.0.0.1`, would otherwise have the relay handed out to nowhere while the log says "resolved": such an answer
/// is a failed lookup.
fn public(ip: Ipv4Addr) -> bool {
    let shared = ip.octets()[0] == 100 && ip.octets()[1] & 0xc0 == 64; // 100.64.0.0/10, carrier-grade NAT
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || shared)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 1);
    const B: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 2);

    fn secs(start: Instant, s: u64) -> Instant {
        start + Duration::from_secs(s)
    }

    #[test]
    fn only_a_public_address_can_be_handed_out() {
        for ip in [
            [192, 168, 1, 50],
            [10, 0, 0, 1],
            [172, 16, 0, 1],
            [127, 0, 0, 1],
            [169, 254, 1, 1],
            [100, 64, 0, 1],
            [100, 127, 255, 1],
            [0, 0, 0, 0],
            [255, 255, 255, 255],
            [224, 0, 0, 1],
        ] {
            assert!(!public(ip.into()), "{ip:?}");
        }
        // The edges of the shared range, and the documentation range the tests answer with.
        for ip in [
            [100, 63, 0, 1],
            [100, 128, 0, 1],
            [203, 0, 113, 7],
            [51, 154, 57, 235],
        ] {
            assert!(public(ip.into()), "{ip:?}");
        }
    }

    #[test]
    fn nothing_is_known_until_a_lookup() {
        let t = Instant::now();
        assert_eq!(Address::default().step(t), Step::Lookup);
    }

    #[test]
    fn an_address_is_used_for_five_minutes_then_looked_up_again() {
        let t = Instant::now();
        let mut a = Address::default();
        a.found(A, t);
        assert_eq!(a.step(t), Step::Use(Some(A)));
        assert_eq!(a.step(secs(t, 299)), Step::Use(Some(A)));
        assert_eq!(a.step(secs(t, 300)), Step::Lookup);
        assert_eq!(a.step(secs(t, 3600)), Step::Lookup);
    }

    #[test]
    fn a_failure_keeps_the_last_address_and_waits_thirty_seconds() {
        let t = Instant::now();
        let mut a = Address::default();
        a.found(A, t);
        let later = secs(t, 400);
        assert_eq!(a.step(later), Step::Lookup, "stale");
        assert_eq!(a.failed(later), Some(A), "the address kept");
        assert_eq!(a.step(secs(later, 29)), Step::Use(Some(A)));
        assert_eq!(a.step(secs(later, 30)), Step::Lookup, "tried again");
        // Failing again, however long it has lasted: still the last good one.
        assert_eq!(a.failed(secs(later, 3000)), Some(A));
        assert_eq!(a.step(secs(later, 3001)), Step::Use(Some(A)));
    }

    #[test]
    fn a_failure_with_nothing_known_connects_directly_and_waits() {
        let t = Instant::now();
        let mut a = Address::default();
        assert_eq!(a.failed(t), None);
        assert_eq!(a.step(secs(t, 29)), Step::Use(None));
        assert_eq!(a.step(secs(t, 30)), Step::Lookup);
    }

    #[test]
    fn a_success_replaces_the_address_and_ends_the_wait() {
        let t = Instant::now();
        let mut a = Address::default();
        assert_eq!(a.found(A, t), None, "the first one replaces nothing");
        let later = secs(t, 400);
        a.failed(later);
        assert_eq!(a.found(B, secs(later, 30)), Some(A), "it changed");
        assert_eq!(a.step(secs(later, 31)), Step::Use(Some(B)));
        assert_eq!(a.found(B, secs(later, 400)), Some(B), "the same again");
    }
}
