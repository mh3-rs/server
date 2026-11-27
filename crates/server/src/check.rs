//! `mh3sp check <domain>`: what an operator runs from another machine to learn which port forward or DNS record is
//! missing (`../docs/history/reachability.md`). It reads no configuration: it plays the part of a console, through the
//! names the patch makes the game look up, on the ports the game calls.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::pin::Pin;
use std::time::Duration;

use bytes::Bytes;
use futures_util::future::join_all;
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket, lookup_host};
use tokio::time::{Instant, timeout, timeout_at};
use tokio_openssl::SslStream;
use tokio_util::codec::Framed;
use wire::natneg::{AVAILABLE, Body, Record};
use wire::{Frame, FrameCodec, healthcheck};

/// Per check. A UDP probe sends twice within it (two waits of 2 s: a datagram may be lost).
const TIMEOUT: Duration = Duration::from_secs(5);

/// The ports a check goes to: the game's own, but for the tests, which run the server on random ones.
#[derive(Clone, Copy)]
pub struct Ports {
    pub nas: u16,
    pub opn: u16,
    pub lmp: u16,
    pub fmp: u16,
    pub qr: u16,
    pub natneg: u16,
}

impl Ports {
    /// The defaults (`mh3sp.toml.example`): OPN, QR and NatNeg's are the game's own, fixed in its patch.
    pub const GAME: Ports = Ports {
        nas: 80,
        opn: 8200,
        lmp: 8201,
        fmp: 8202,
        qr: 27900,
        natneg: 27901,
    };
}

/// One check's result.
pub struct Line {
    what: String,
    /// What to fix; `None` when it passed.
    failure: Option<String>,
}

impl Line {
    fn ok(what: String) -> Self {
        Line {
            what,
            failure: None,
        }
    }

    fn fail(what: String, fix: String) -> Self {
        Line {
            what,
            failure: Some(fix),
        }
    }
}

/// Every check's result, in order.
pub struct Report(pub Vec<Line>);

impl Report {
    pub fn passed(&self) -> bool {
        self.0.iter().all(|l| l.failure.is_none())
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for line in &self.0 {
            match &line.failure {
                None => writeln!(f, "ok    {}", line.what)?,
                Some(fix) => writeln!(f, "FAIL  {}: {fix}", line.what)?,
            }
        }
        write!(
            f,
            "      relay ports 40000-40099 (UDP): not checked, forward them"
        )
    }
}

/// Resolves the patch's names, compares them, then probes each service. The caller prints the report and exits 1
/// unless it [`passed`](Report::passed).
pub async fn run(domain: &str) -> Report {
    let names = crate::patch::records(domain);
    let looked_up = join_all(names.iter().map(|n| lookup(n))).await;
    let mut lines = agreement(&names, &looked_up);
    // A name that doesn't resolve has its line already: nothing to probe through it.
    let addresses: Vec<Option<Ipv4Addr>> = looked_up
        .iter()
        .map(|r| r.as_ref().ok().and_then(|a| a.first().copied()))
        .collect();
    lines.extend(probe(domain, &addresses, &Ports::GAME).await);
    Report(lines)
}

/// The A records of `name`, IPv4 only (the Wii speaks no IPv6), sorted.
async fn lookup(name: &str) -> Result<Vec<Ipv4Addr>, String> {
    let found = timeout(TIMEOUT, lookup_host((name, 0)))
        .await
        .map_err(|_| "the lookup timed out".to_owned())?
        .map_err(|e| e.to_string())?;
    let mut addresses: Vec<Ipv4Addr> = found
        .filter_map(|a| match a.ip() {
            IpAddr::V4(ip) => Some(ip),
            IpAddr::V6(_) => None,
        })
        .collect();
    addresses.sort();
    addresses.dedup();
    if addresses.is_empty() {
        return Err("no IPv4 address".to_owned());
    }
    Ok(addresses)
}

/// Every name resolves; the first four (the domain, OPN, the availability check, NatNeg's first server) to the same
/// addresses as the first that does, since the patch points them at one server. `b` and `c` only have to resolve:
/// they may point at a second IP of the same server (`deploy/README.md`, "Hostnames").
pub fn agreement(names: &[String], looked_up: &[Result<Vec<Ipv4Addr>, String>]) -> Vec<Line> {
    const MUST_AGREE: usize = 4;
    let reference = looked_up
        .iter()
        .zip(names)
        .take(MUST_AGREE)
        .find_map(|(r, n)| r.as_ref().ok().map(|a| (n, a)));
    names
        .iter()
        .zip(looked_up)
        .enumerate()
        .map(|(i, (name, result))| {
            let what = format!("DNS {name}");
            match (result, reference) {
                (Err(e), _) => Line::fail(what, format!("create its A record ({e})")),
                (Ok(addresses), Some((first, expected)))
                    if i < MUST_AGREE && addresses != expected =>
                {
                    Line::fail(
                        what,
                        format!(
                            "resolves to {} but {first} to {}: point it at the server",
                            join(addresses),
                            join(expected)
                        ),
                    )
                }
                (Ok(_), _) => Line::ok(what),
            }
        })
        .collect()
}

fn join(addresses: &[Ipv4Addr]) -> String {
    let list: Vec<String> = addresses.iter().map(ToString::to_string).collect();
    list.join(", ")
}

/// Probes every service. `addresses` is each name's address in the order of [`crate::patch::records`] (`None`:
/// unresolved, so skipped).
pub async fn probe(domain: &str, addresses: &[Option<Ipv4Addr>], ports: &Ports) -> Vec<Line> {
    let names = crate::patch::records(domain);
    let at = |i: usize, port: u16| addresses[i].map(|ip| SocketAddrV4::new(ip, port));
    let probes = [
        (0, ports.nas, Kind::Http, "Nintendo login"),
        (1, ports.opn, Kind::Pat { tls: true }, "OPN"),
        (0, ports.lmp, Kind::Pat { tls: false }, "LMP"),
        (0, ports.fmp, Kind::Pat { tls: false }, "FMP"),
        (2, ports.qr, Kind::Qr, "availability check"),
        (3, ports.natneg, Kind::Natneg, "NAT negotiation"),
    ];
    join_all(probes.into_iter().filter_map(|(i, port, kind, role)| {
        let target = at(i, port)?;
        let what = format!("{} {port} {} ({role})", kind.protocol(), names[i]);
        let domain = domain.to_owned();
        Some(async move {
            match kind.probe(target, &domain).await {
                Ok(None) => Line::ok(what),
                Ok(Some(note)) => Line::ok(format!("{what}: {note}")),
                Err(e) => Line::fail(what, format!("{} ({e})", kind.fix(port))),
            }
        })
    }))
    .await
}

#[derive(Clone, Copy)]
enum Kind {
    Http,
    /// OPN (over TLS), LMP and FMP: the healthcheck.
    Pat {
        tls: bool,
    },
    Qr,
    /// The healthcheck, in NAT negotiation's records.
    Natneg,
}

/// How a probe passed: with nothing to add, or with a note on the line.
type Passed = Option<&'static str>;

impl Kind {
    fn protocol(self) -> &'static str {
        match self {
            Kind::Http | Kind::Pat { .. } => "TCP",
            Kind::Qr | Kind::Natneg => "UDP",
        }
    }

    fn fix(self, port: u16) -> String {
        match self {
            Kind::Http => format!("forward TCP {port} to the server (or to your reverse proxy)"),
            Kind::Pat { .. } => format!("forward TCP {port} to the server"),
            Kind::Qr | Kind::Natneg => format!("forward UDP {port} to the server"),
        }
    }

    async fn probe(self, target: SocketAddrV4, domain: &str) -> Result<Passed, String> {
        let done = match self {
            Kind::Http => {
                timeout(TIMEOUT, async { http(target, domain).await.map(|()| None) }).await
            }
            Kind::Pat { tls } => timeout(TIMEOUT, pat(target, domain, tls)).await,
            Kind::Qr => timeout(TIMEOUT, async { qr(target).await.map(|()| None) }).await,
            Kind::Natneg => timeout(TIMEOUT, async { natneg(target).await.map(|()| None) }).await,
        };
        done.unwrap_or_else(|_| Err("timed out".to_owned()))
    }
}

/// OPN, LMP or FMP: connects (and shakes hands, on OPN), sends the healthcheck as the first frame, and reads what
/// comes back.
async fn pat(target: SocketAddrV4, domain: &str, tls: bool) -> Result<Passed, String> {
    let tcp = TcpStream::connect(target)
        .await
        .map_err(|e| e.to_string())?;
    if !tls {
        return healthcheck_frame(tcp).await;
    }
    let ssl = wire::tls::wii_client()
        .and_then(|client| client.configure())
        .and_then(|config| config.into_ssl(domain))
        .map_err(|e| e.to_string())?;
    let mut stream = SslStream::new(ssl, tcp).map_err(|e| e.to_string())?;
    Pin::new(&mut stream)
        .connect()
        .await
        .map_err(|e| format!("TLS handshake failed: {e}"))?;
    healthcheck_frame(stream).await
}

/// The server answers a healthcheck with the same `seq`. A server older than the check sends its own opening instead
/// (a request of the game): it is reached, which is what the check is for, but it can't tell us more.
async fn healthcheck_frame(stream: impl AsyncRead + AsyncWrite + Unpin) -> Result<Passed, String> {
    const SEQ: u16 = 1;
    let mut framed = Framed::new(stream, FrameCodec);
    let request = Frame {
        seq: SEQ,
        id: healthcheck::REQUEST,
        payload: Bytes::new(),
    };
    framed.send(request).await.map_err(|e| e.to_string())?;
    match framed.next().await {
        Some(Ok(f)) if f.id == healthcheck::ANSWER && f.seq == SEQ => Ok(None),
        Some(Ok(f)) if f.id.entry().is_some() => Ok(Some("older server: no healthcheck")),
        Some(Ok(_)) => Err("an answer that isn't the server's".to_owned()),
        Some(Err(e)) => Err(e.to_string()),
        None => Err("closed without an answer".to_owned()),
    }
}

/// NAT negotiation's healthcheck, sent again after a while like the availability check's, and answered only from the
/// address it went to, with its own cookie: anything else is not our answer.
async fn natneg(target: SocketAddrV4) -> Result<(), String> {
    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .map_err(|e| e.to_string())?;
    let mut cookie = [0; 4];
    openssl::rand::rand_bytes(&mut cookie).map_err(|e| e.to_string())?;
    let cookie = u32::from_be_bytes(cookie);
    let request = Record {
        cookie,
        body: Body::Healthcheck,
    }
    .encode();
    let mut buf = [0; 1500];
    for _ in 0..2 {
        socket
            .send_to(&request, SocketAddr::V4(target))
            .await
            .map_err(|e| e.to_string())?;
        let until = Instant::now() + Duration::from_secs(2);
        while let Ok(received) = timeout_at(until, socket.recv_from(&mut buf)).await {
            let (n, from) = received.map_err(|e| e.to_string())?;
            if from == SocketAddr::V4(target)
                && matches!(Record::decode(&buf[..n]),
                    Ok(r) if r.cookie == cookie && r.body == Body::HealthcheckAnswer)
            {
                return Ok(());
            }
        }
    }
    Err("no answer: not forwarded, or a server older than this check".to_owned())
}

/// The Nintendo login must answer a `login` form (the game's own second call, which changes nothing) with 200 and a
/// form that has its `returncd`. A proxy's error page, another service on the port, or the guide (which is on the
/// public port now) fails with what it said.
async fn http(target: SocketAddrV4, domain: &str) -> Result<(), String> {
    let form = wire::nas::encode(&[("action", b"login")]);
    let request = format!(
        "POST /ac HTTP/1.1\r\nHost: {domain}\r\nContent-Type: application/x-www-form-urlencoded\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{form}",
        form.len()
    );
    let mut stream = TcpStream::connect(target)
        .await
        .map_err(|e| e.to_string())?;
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    // The answer is a few dozen bytes; what a proxy's page adds is cut.
    let mut response = Vec::new();
    let mut buf = [0; 512];
    while response.len() < 4096 {
        match stream.read(&mut buf).await.map_err(|e| e.to_string())? {
            0 => break,
            n => response.extend_from_slice(&buf[..n]),
        }
    }
    let response = String::from_utf8_lossy(&response);
    let line = response.lines().next().unwrap_or_default();
    if line.split_whitespace().nth(1) != Some("200") {
        return Err(format!("answered {line:?}"));
    }
    match response.contains("returncd=") {
        true => Ok(()),
        false => Err("answered 200, but not as the Nintendo login does".to_owned()),
    }
}

/// The availability check, sent again after a while: UDP loses datagrams, and the answer to a repeat is the same.
async fn qr(target: SocketAddrV4) -> Result<(), String> {
    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .map_err(|e| e.to_string())?;
    let request = b"\x09\0\0\0\0mh3uswii\0";
    let mut buf = [0; 1500];
    for _ in 0..2 {
        socket
            .send_to(request, SocketAddr::V4(target))
            .await
            .map_err(|e| e.to_string())?;
        let Ok(received) = timeout(Duration::from_secs(2), socket.recv_from(&mut buf)).await else {
            continue;
        };
        let (n, from) = received.map_err(|e| e.to_string())?;
        if from != SocketAddr::V4(target) {
            continue;
        }
        return if buf[..n] == AVAILABLE {
            Ok(())
        } else {
            Err("an answer that isn't the server's".to_owned())
        };
    }
    Err("no answer".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(last: u8) -> Result<Vec<Ipv4Addr>, String> {
        Ok(vec![Ipv4Addr::new(203, 0, 113, last)])
    }

    fn names() -> Vec<String> {
        crate::patch::records("mh3.example.org")
    }

    fn failures(lines: &[Line]) -> Vec<(&str, &str)> {
        lines
            .iter()
            .filter_map(|l| l.failure.as_deref().map(|f| (l.what.as_str(), f)))
            .collect()
    }

    #[test]
    fn six_names_at_one_address_agree() {
        let lines = agreement(&names(), &vec![ip(10); 6]);
        assert_eq!(lines.len(), 6);
        assert!(failures(&lines).is_empty());
    }

    #[test]
    fn a_name_elsewhere_fails_naming_it() {
        let mut looked_up = vec![ip(10); 6];
        looked_up[1] = ip(11);
        let lines = agreement(&names(), &looked_up);
        let failed = failures(&lines);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0, "DNS opn.mh3.example.org");
        assert!(failed[0].1.contains("203.0.113.11"), "{}", failed[0].1);
        assert!(failed[0].1.contains("203.0.113.10"), "{}", failed[0].1);
    }

    #[test]
    fn the_last_two_nat_negotiation_names_may_point_at_a_second_address() {
        let mut looked_up = vec![ip(10); 6];
        looked_up[4] = ip(11);
        looked_up[5] = ip(11);
        assert!(failures(&agreement(&names(), &looked_up)).is_empty());
    }

    #[test]
    fn a_missing_record_fails_naming_it() {
        let mut looked_up = vec![ip(10); 6];
        looked_up[4] = Err("no such host".into());
        let lines = agreement(&names(), &looked_up);
        let failed = failures(&lines);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0, "DNS mh3uswii.b.mh3.example.org");
        assert!(
            failed[0].1.contains("create its A record"),
            "{}",
            failed[0].1
        );
    }

    #[test]
    fn the_domain_itself_missing_leaves_the_others_to_agree_among_themselves() {
        let mut looked_up = vec![ip(10); 6];
        looked_up[0] = Err("no such host".into());
        // The reference falls to the first name that resolves (OPN): the availability name disagrees with it.
        looked_up[2] = ip(11);
        let lines = agreement(&names(), &looked_up);
        let failed = failures(&lines);
        assert_eq!(failed.len(), 2, "{failed:?}");
        assert_eq!(failed[0].0, "DNS mh3.example.org");
        assert_eq!(failed[1].0, "DNS mh3uswii.available.mh3.example.org");
    }

    /// `probe` reaches the names by their place in `patch::records` (0 the domain, 1 OPN, 2 the availability check).
    #[test]
    fn the_probes_rely_on_the_order_of_the_patchs_records() {
        let names = names();
        assert_eq!(names[0], "mh3.example.org");
        assert_eq!(names[1], "opn.mh3.example.org");
        assert_eq!(names[2], "mh3uswii.available.mh3.example.org");
        assert_eq!(names.len(), 6);
    }

    /// `.invalid` never resolves (RFC 6761): the lookup fails instead of passing on whatever the resolver makes up.
    #[tokio::test]
    async fn a_name_that_cannot_resolve_fails_its_lookup() {
        assert!(lookup("mh3.example.invalid").await.is_err());
    }
}
