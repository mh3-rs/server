//! The Rust server, started in-process on random ports, against the scripted console and the corpus.
//! Each server gets its own throw-away SQLite file.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bytes::{Buf, BufMut, Bytes};
use server::Config;
use testkit::client::{Console, Received};
use testkit::corpus;
use testkit::masks::Masks;
use testkit::replay::{self, ConnectionReport, Targets};
use wire::packets::*;
use wire::{Frame, Item, PatData};

/// What the recorded consoles' discs hold (the lab's hosts file points it at AltWFC).
const MH3SP_ANNOUNCE: &str = concat!(
    "<BR><CENTER><BODY><SIZE=6>Monster Hunter 3 (Tri) Server Project<BR><BODY>",
    "<BR><LEFT><SIZE=4>Welcome to the MH3SP Beta Test! We ask that you<BR><BODY>",
    "keep few things in mind while connected in order to<BR><BODY>",
    "make the experience as fun as possible for everyone.<BR><BODY>",
    "No abusing other players, no exploiting bugs, and<BR><BODY>",
    "please report bugs that you find to the server<BR><BODY>",
    "staff/dev team. We thank you for your patience as<BR><BODY>",
    "we prepared this server to be fit for public use,<BR><BODY>",
    "and ask your continued patience in the weeks to<BR><BODY>",
    "come as you may encounter some bugs and errors<BR><BODY>",
    "while playing. Please see the Public Test Server<BR><BODY>",
    "Roadmap for a list of features we are working on.<BR><BODY>",
    "<BR><CENTER><C=2>Welcome to Loc Lac!<END>",
);
const MH3SP_GUILDMASTER: &str = concat!(
    "To all hunters:\n",
    "Loc Lac is in \"beta,\"\n",
    "whatever that means. Also,\n",
    "the Arena is back in\n",
    "operation! And now, a:\n",
    "haiku: \"Patient warriors /\n",
    "returning home at long last /\n",
    "now join hands to hunt!\"",
);
/// The tests' `domain`.
const DOMAIN: &str = "mh3.example.org";
/// What OPN reads on a disc with our patch for [`DOMAIN`].
static NATNEG_HOSTNAMES: std::sync::LazyLock<Vec<u8>> =
    std::sync::LazyLock::new(|| testkit::replay::natneg_memory(DOMAIN));

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The commands' sockets of the tests' servers, each its own: they run side by side in one network namespace.
static CONTROL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A server config on random ports, with its own database file in a temporary folder: tests never share
/// state (or a schema version), and the folder is deleted when the returned guard is dropped. A real file, not
/// `:memory:`, so tests run the same SQLite mode as production (WAL, several connections).
fn config() -> (Config, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let any: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let config = Config {
        opn: any,
        lmp: any,
        fmp: any,
        public: any,
        nas: any,
        qr: any,
        natneg: any,
        strategy: server::config::Strategy::P2p,
        relay_ports: 41000..=41099,
        tls_dir: dir.path().join("tls"),
        database_url: format!("sqlite://{}", dir.path().join("mh3sp.db").display()),
        // Where most recordings were made; a replay uses its scenario's own (`recorded_address`).
        external_address: Some("192.168.110.150".into()),
        domain: Some(DOMAIN.into()),
        secret: Some(server::config::Secret(b"test secret".to_vec())),
        // MH3SP's event files (`scripts/ci.sh` provides them).
        content_dir: std::env::var_os("CONTENT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| repo().join("../reference")),
        encryption: false,
        admins: Vec::new(),
        name: "Test Server".into(),
        // MH3SP's texts: the corpus recorded them.
        announce: MH3SP_ANNOUNCE.into(),
        no_charge: "<BODY><CENTER>MH3 Server Project - No charge.<END>".into(),
        terms: "MH3 Server Project - Terms.".into(),
        subterms: "MH3 Server Project - SubTerms.".into(),
        guildmaster: MH3SP_GUILDMASTER.into(),
        ignored: Vec::new(),
        control: format!(
            "mh3sp-test-{}-{}",
            std::process::id(),
            CONTROL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ),
        clock_offset: 0,
        // Replays send faster than any console: `a_chat_flood_is_cut_to_its_rate` turns them on.
        rate_limits: false,
        loopback_exempt: true,
        maintenance: None,
    };
    (config, dir)
}

async fn http_status(addr: SocketAddr, path: &str) -> String {
    http_get(addr, path).await.0
}

/// A `GET` on the public port: status code and body.
async fn http_get(addr: SocketAddr, path: &str) -> (String, String) {
    let (status, body) = http_bytes(addr, path).await;
    (status, String::from_utf8(body).unwrap())
}

/// A `GET`: status code and body as bytes (a `.gct` isn't text).
async fn http_bytes(addr: SocketAddr, path: &str) -> (String, Vec<u8>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.0\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).await.unwrap();
    let end = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&out[..end]);
    let status = head
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    (status, out[end + 4..].to_vec())
}

/// The next frame, which must be a `P`.
async fn expect<P: Packet>(console: &mut Console) -> P {
    match console.recv(Duration::from_secs(5)).await.unwrap() {
        Received::Frame(f) if f.id == P::ID => P::decode(&f.payload).unwrap(),
        other => panic!("expected {}, got {other:?}", P::ID),
    }
}

async fn send<P: Packet>(console: &mut Console, seq: u16, packet: P) {
    let mut payload = Vec::new();
    packet.encode(&mut payload).unwrap();
    let frame = Frame {
        seq,
        id: P::ID,
        payload: payload.into(),
    };
    console.send(frame).await.unwrap();
}

/// A console logging in to OPN with this disc: its `NtcLogin` status, and the refusal text when refused.
async fn opn_login(
    opn: SocketAddr,
    game_id: &[u8],
    natneg_hostnames: &[u8],
) -> (u8, Option<String>) {
    let (mut console, status) = opn_connect(opn, game_id, natneg_hostnames).await;
    if status != 1 {
        return (status, None);
    }
    // Refused, the game sends its token anyway, then asks why (`PatInterface.cpp:1250-1275`).
    let token = Bytes::from_static(b"NDS-from-any-login");
    send(&mut console, 1, ReqAuthenticationToken { token }).await;
    expect::<AnsAuthenticationToken>(&mut console).await;
    (status, Some(refusal_text(&mut console, 2).await))
}

/// After a refused `NtcLogin`: the text `ReqMaintenance` gets.
async fn refusal_text(console: &mut Console, seq: u16) -> String {
    send(console, seq, ReqMaintenance {}).await;
    let text = expect::<AnsMaintenance>(console).await.text;
    String::from_utf8(text.to_vec()).unwrap()
}

/// A console through OPN's disc checks and connection: the connection and its `NtcLogin` status.
async fn opn_connect(opn: SocketAddr, game_id: &[u8], natneg_hostnames: &[u8]) -> (Console, u8) {
    let mut console = Console::connect(opn, true).await.unwrap();
    let mut memory = game_id;
    loop {
        let check = expect::<ReqMemoryCheck>(&mut console).await;
        send(
            &mut console,
            0,
            AnsMemoryCheck {
                data: PatData::new(),
                address: check.address,
                memory: memory.to_vec().into(),
            },
        )
        .await;
        if memory == natneg_hostnames || !matches!(game_id, b"RMHP08" | b"RMHE08") {
            break;
        }
        memory = natneg_hostnames;
    }
    expect::<ReqConnection>(&mut console).await;
    send(
        &mut console,
        0,
        AnsConnection {
            data: PatData::new(),
        },
    )
    .await;
    let status = expect::<NtcLogin>(&mut console).await.status;
    (console, status)
}

/// With encryption on, each console gets its own key at OPN, and every LMP and FMP connection of that console is
/// encrypted with it from its first frame: the server opens with the newest key in transit from the console's IP,
/// then reads the answer with that IP's keys. Two consoles behind one IP, crossing; an address without a key closed.
#[tokio::test]
async fn encrypted_login() {
    let (mut config, _db) = config();
    config.encryption = true;
    let running = server::start(&config).await.unwrap();
    let (lmp, fmp) = (Lmp::of(&running), running.fmp.unwrap());
    // OPN, for two consoles at the same address: each its key, which goes on at `ReqShut`.
    let mut keys = Vec::new();
    for _ in 0..2 {
        let (mut opn, _) = opn_connect(running.opn, b"RMHP08", &NATNEG_HOSTNAMES).await;
        send(&mut opn, 1, ReqCommonKey {}).await;
        let key: [u8; 32] = expect::<AnsCommonKey>(&mut opn).await.key[..]
            .try_into()
            .unwrap();
        send(&mut opn, 2, ReqShut { kind: 2 }).await;
        expect::<AnsShut>(&mut opn).await;
        keys.push(key);
    }
    assert_ne!(keys[0], keys[1]);

    // The first console first, while the second's key is the newest at their address: its opening comes with the
    // second's key, its answer is read with its own.
    for key in [keys[0], keys[1]] {
        // LMP's first visit: a new console gets its support code and a ticket.
        let mut console = open_encrypted(lmp.addr, key, None, 1).await;
        let data = login_data(lmp.nas).await;
        send(&mut console, 1, ReqLoginInfo { data }).await;
        expect::<AnsLoginInfo>(&mut console).await;
        let first = ticket_and_shut(&mut console, 2).await;
        // LMP's second visit: a slot, a Capcom ID, the ticket for FMP.
        let mut console = open_encrypted(lmp.addr, key, Some(first), 2).await;
        let user = PatData::new().with(0x03, Item::String(Bytes::from_static(b"Aiko")));
        send(
            &mut console,
            1,
            ReqUserObject {
                unk: 1,
                slot: 1,
                user,
            },
        )
        .await;
        expect::<AnsUserObject>(&mut console).await;
        let second = ticket_and_shut(&mut console, 2).await;
        // FMP, with the same key.
        open_encrypted(fmp, key, Some(second), 3).await;
    }

    // No key in transit at an address (every one used): the connection is closed before a word.
    let mut stranger = Console::connect(lmp.addr, false).await.unwrap();
    assert!(matches!(
        stranger.recv(Duration::from_secs(5)).await.unwrap(),
        Received::Closed
    ));
    running.stop().await;
}

/// An encrypted LMP or FMP connection: the opening, then the console's answer with its key; `NtcLogin`'s status.
async fn open_encrypted(
    addr: SocketAddr,
    key: [u8; 32],
    ticket: Option<String>,
    status: u8,
) -> Console {
    // The opening may come with another console's key (two in transit at one IP): ignored, as the game does.
    let mut console = Console::connect(addr, false).await.unwrap();
    match console.recv(Duration::from_secs(5)).await.unwrap() {
        Received::Frame(f) if f.id == ReqConnection::ID => {}
        other => panic!("expected the opening, got {other:?}"),
    }
    let mut console = console.encrypted(key);
    let data = connection_data(None, ticket.as_deref());
    send(&mut console, 0, AnsConnection { data }).await;
    assert_eq!(expect::<NtcLogin>(&mut console).await.status, status);
    console
}

/// A new ticket, then the console leaves for its next connection.
async fn ticket_and_shut(console: &mut Console, seq: u16) -> String {
    send(console, seq, ReqTicket {}).await;
    let ticket = expect::<AnsTicket>(console).await.ticket;
    send(console, seq + 1, ReqShut { kind: 2 }).await;
    expect::<AnsShut>(console).await;
    String::from_utf8(ticket.to_vec()).unwrap()
}

/// With encryption on, OPN gives the console a 32-byte key when asked; off, it skips encryption as MH3SP does.
#[tokio::test]
async fn common_key_only_with_encryption() {
    for encryption in [false, true] {
        let (mut config, _db) = config();
        config.encryption = encryption;
        let running = server::start(&config).await.unwrap();
        let (mut console, status) = opn_connect(running.opn, b"RMHP08", &NATNEG_HOSTNAMES).await;
        assert_eq!(status, 5);
        send(&mut console, 1, ReqCommonKey {}).await;
        match encryption {
            true => assert_eq!(expect::<AnsCommonKey>(&mut console).await.key.len(), 32),
            false => {
                expect::<AnsAuthenticationToken>(&mut console).await;
            }
        }
        running.stop().await;
    }
}

#[tokio::test]
async fn accepts_wii_tls_and_reports_health() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();

    // OPN: the scripted console speaks TLS like a Wii (TLS 1.0, AES128-SHA); after the pause, the disc check.
    let mut opn = Console::connect(running.opn, true).await.unwrap();
    let check = expect::<ReqMemoryCheck>(&mut opn).await;
    assert_eq!((check.address, check.size), (0x8000_0000, 6));
    Console::connect(running.lmp.unwrap(), false).await.unwrap();

    assert_eq!(http_status(running.public, "/health/live").await, "200");
    assert_eq!(http_status(running.public, "/health/ready").await, "200");
    assert_eq!(http_status(running.public, "/elsewhere").await, "404");
    // `mh3sp healthcheck` is blocking (it runs as its own process in the container).
    let public = running.public;
    tokio::task::spawn_blocking(move || server::public::check(public))
        .await
        .unwrap()
        .unwrap();

    running.stop().await;
}

/// The payload of the scenario's first `name` frame.
fn first_payload(scenario: &Path, name: &str) -> Option<Vec<u8>> {
    let recordings = corpus::load(scenario).unwrap();
    let frame = recordings
        .iter()
        .flat_map(|r| r.connections.iter().flat_map(|c| &c.frames))
        .filter(|f| f.name == name)
        .min_by_key(|f| f.t)?;
    Some(hex::decode(&frame.payload).unwrap())
}

/// When the scenario was recorded, by the reference server's clock (its first `AnsServerTime`).
fn recorded_at(scenario: &Path) -> Option<i64> {
    let payload = first_payload(scenario, "AnsServerTime")?;
    Some(i64::from((&payload[..]).get_u32()))
}

/// The address the reference server handed out when the scenario was recorded (its first `AnsLmpConnect`).
fn recorded_address(scenario: &Path) -> Option<String> {
    let payload = first_payload(scenario, "AnsLmpConnect")?;
    let mut p = &payload[..];
    let len = p.get_u16() as usize;
    Some(String::from_utf8_lossy(&p[..len]).into_owned())
}

async fn replay(scenario: &Path) -> Vec<ConnectionReport> {
    let masks = Masks::load(&repo().join("corpus")).unwrap();
    // A fresh server per scenario, as when it was recorded, and at its date (content depends on it).
    let (mut config, _db) = config();
    if let Some(at) = recorded_at(scenario) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        config.clock_offset = at - now;
    }
    if let Some(address) = recorded_address(scenario) {
        config.external_address = Some(address);
    }
    let running = server::start(&config).await.unwrap();
    let targets = Targets {
        opn: running.opn,
        lmp: running.lmp.unwrap(),
        fmp: running.fmp.unwrap(),
        nas: Some(running.nas),
        domain: Some(DOMAIN.into()),
        nudge: false,
    };
    let reports = replay::replay(scenario, &targets, &masks).await.unwrap();
    running.stop().await;
    reports
}

fn scenarios() -> Vec<PathBuf> {
    let mut scenarios: Vec<_> = std::fs::read_dir(repo().join("corpus"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.join("raw").is_dir())
        .collect();
    scenarios.sort();
    scenarios
}

fn assert_clean(scenario: &Path, reports: &[ConnectionReport]) {
    let dirty: Vec<_> = reports.iter().filter(|r| !r.is_clean()).collect();
    assert!(dirty.is_empty(), "{}: {dirty:#?}", scenario.display());
}

/// Tier 2: scenarios whose milestone is reached must replay cleanly against the Rust server.
#[tokio::test]
async fn reached_scenarios_replay_cleanly() {
    for scenario in scenarios() {
        if corpus::milestone(&scenario).unwrap().as_str() > corpus::REACHED {
            continue;
        }
        assert_clean(&scenario, &replay(&scenario).await);
    }
}

/// The replay gate so far: the login connections (OPN, LMP) and FMP's server selection (the connection that picks a
/// world server, `ReqFmpInfo2`) of every scenario, whatever its milestone.
#[tokio::test]
async fn reached_connections_replay_cleanly() {
    for scenario in scenarios() {
        let selection: Vec<String> = corpus::load(&scenario)
            .unwrap()
            .iter()
            .flat_map(|rec| {
                rec.connections
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.frames.iter().any(|f| f.name == "ReqFmpInfo2"))
                    .map(|(n, c)| format!("{}-{:02}-{}", rec.console, n + 1, c.role))
            })
            .collect();
        let reports = replay(&scenario).await;
        let reached: Vec<_> = reports
            .into_iter()
            .filter(|r| {
                r.name.ends_with("-opn") || r.name.ends_with("-lmp") || selection.contains(&r.name)
            })
            .collect();
        assert_clean(&scenario, &reached);
    }
}

#[tokio::test]
async fn refuses_discs_without_our_patch() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let opn = running.opn;
    let (ok, unpatched, unsupported) = tokio::join!(
        opn_login(opn, b"RMHE08", &NATNEG_HOSTNAMES),
        opn_login(
            opn,
            b"RMHP08",
            b"natneg1.gs.nintendowifi.net\0natneg2.gs.nintendowifi.net\0"
        ),
        opn_login(opn, b"RMHJ08", b""),
    );
    running.stop().await;
    assert_eq!(ok, (5, None));
    assert_eq!(unpatched.0, 1);
    let unpatched = unpatched.1.unwrap();
    assert!(unpatched.contains("not patched"), "{unpatched}");
    assert!(
        unpatched.contains("<SIZE=6>Test Server<"),
        "the configured name heads it"
    );
    assert_eq!(unsupported.0, 1);
    assert!(unsupported.1.unwrap().contains("not supported"));
}

/// The CA a written patch carries: the value written at the PAL CA's address.
fn patch_ca(patch: &Path) -> openssl::x509::X509 {
    let xml =
        std::fs::read_to_string(patch.join(format!("riivolution/{DOMAIN}-RMHP08.xml"))).unwrap();
    let hex = xml
        .split("offset=\"0x80572840\" value=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap();
    openssl::x509::X509::from_der(&hex::decode(hex).unwrap()).unwrap()
}

/// What a patched console checks: OPN's certificate comes from the CA the patch carries, under the name the game
/// asks for. Every other test's client checks nothing (`client.rs`).
#[tokio::test]
async fn opn_s_certificate_verifies_against_the_patch_s_ca() {
    use openssl::ssl::{SslConnector, SslMethod, SslVerifyMode, SslVersion};
    let (config, dir) = config();
    let running = server::start(&config).await.unwrap();
    let patch = dir.path().join("patch");
    server::patch::write(&patch, DOMAIN, &config.tls_dir).unwrap();
    let ca = patch_ca(&patch);
    let handshake = |ca: openssl::x509::X509, name: &'static str| async move {
        let mut builder = SslConnector::builder(SslMethod::tls_client()).unwrap();
        builder.set_security_level(0);
        builder
            .set_max_proto_version(Some(SslVersion::TLS1))
            .unwrap();
        builder.set_cipher_list("AES128-SHA:@SECLEVEL=0").unwrap();
        builder.set_verify(SslVerifyMode::PEER);
        builder.cert_store_mut().add_cert(ca).unwrap();
        let ssl = builder.build().configure().unwrap().into_ssl(name).unwrap();
        let tcp = tokio::net::TcpStream::connect(running.opn).await.unwrap();
        let mut stream = tokio_openssl::SslStream::new(ssl, tcp).unwrap();
        std::pin::Pin::new(&mut stream).connect().await.is_ok()
    };
    assert!(handshake(ca.clone(), "mh.capcom.co.jp").await);
    assert!(
        !handshake(ca, "opn.mh3.example.org").await,
        "the name the game checks, not the host"
    );
    let other = tempfile::tempdir().unwrap();
    server::patch::write(
        &other.path().join("patch"),
        DOMAIN,
        &other.path().join("tls"),
    )
    .unwrap();
    let other_ca = patch_ca(&other.path().join("patch"));
    assert!(
        !handshake(other_ca, "mh.capcom.co.jp").await,
        "another deployment's patch"
    );
    running.stop().await;
}

/// What `mh3sp check` sees of a server on `127.0.0.1`: the game's ports replaced by the server's random ones, `port`
/// swapped in by the test where it breaks one.
async fn check_report(
    running: &server::Running,
    change: impl FnOnce(&mut server::check::Ports),
) -> String {
    let mut ports = server::check::Ports {
        nas: running.nas.port(),
        opn: running.opn.port(),
        lmp: running.lmp.unwrap().port(),
        fmp: running.fmp.unwrap().port(),
        qr: running.qr.unwrap().port(),
    };
    change(&mut ports);
    let here = Some(std::net::Ipv4Addr::LOCALHOST);
    let report = server::check::Report(server::check::probe(DOMAIN, &[here; 6], &ports).await);
    format!("{report}\npassed={}", report.passed())
}

/// The `FAIL` lines of a `check_report`.
fn check_failures(report: &str) -> Vec<&str> {
    report.lines().filter(|l| l.starts_with("FAIL")).collect()
}

/// How many checks of a `check_report` passed.
fn check_passes(report: &str) -> usize {
    report.lines().filter(|l| l.starts_with("ok    ")).count()
}

/// `mh3sp check` against a server whose forwards are all in place: every service answers and the run passes.
#[tokio::test]
async fn check_passes_against_a_reachable_server() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let report = check_report(&running, |_| {}).await;
    assert_eq!(check_passes(&report), 5, "{report}");
    assert!(!report.contains("FAIL"), "{report}");
    assert!(report.ends_with("passed=true"), "{report}");
    running.stop().await;
}

/// A closed TCP port fails its own line, with what to forward, and only that one.
#[tokio::test]
async fn check_fails_a_closed_tcp_port() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    // A port nothing listens on: bound, then released.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err(),
        "the port must refuse, or the test proves nothing"
    );
    let report = check_report(&running, |p| p.lmp = port).await;
    let failed = check_failures(&report);
    assert_eq!(failed.len(), 1, "{report}");
    assert!(
        failed[0].contains(&format!("TCP {port} {DOMAIN} (LMP)")),
        "{report}"
    );
    assert!(
        failed[0].contains(&format!("forward TCP {port}")),
        "{report}"
    );
    assert_eq!(check_passes(&report), 4, "{report}");
    assert!(report.ends_with("passed=false"), "{report}");
    running.stop().await;
}

/// A UDP port where nothing answers (a router dropping the datagrams) fails after its timeout, and the others pass.
#[tokio::test]
async fn check_fails_a_silent_udp_port() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    // Bound and held, never read: the datagrams arrive and nothing comes back.
    let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = silent.local_addr().unwrap().port();
    let report = check_report(&running, |p| p.qr = port).await;
    let failed = check_failures(&report);
    assert_eq!(failed.len(), 1, "{report}");
    assert!(failed[0].contains(&format!("UDP {port}")), "{report}");
    assert!(failed[0].contains("forward UDP"), "{report}");
    assert!(failed[0].contains("no answer"), "{report}");
    assert_eq!(check_passes(&report), 4, "{report}");
    drop(silent);
    running.stop().await;
}

/// Port 80 answering something else than the Nintendo login (a proxy's error, another service) fails with what it said.
#[tokio::test]
async fn check_fails_port_80_when_the_login_is_not_served() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    // The public port answers HTTP, but has no `/ac`.
    let report = check_report(&running, |p| p.nas = running.public.port()).await;
    let failed = check_failures(&report);
    assert_eq!(failed.len(), 1, "{report}");
    assert!(failed[0].contains("Nintendo login"), "{report}");
    assert!(failed[0].contains("404"), "{report}");
    assert_eq!(check_passes(&report), 4, "{report}");

    // A service that says 200 to anything isn't the login either.
    let other = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = other.local_addr().unwrap().port();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut stream, _)) = other.accept().await {
            let mut buf = [0; 1024];
            let _ = stream.read(&mut buf).await;
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nwelcome")
                .await;
        }
    });
    let report = check_report(&running, |p| p.nas = port).await;
    let failed = check_failures(&report);
    assert_eq!(failed.len(), 1, "{report}");
    assert!(
        failed[0].contains("not as the Nintendo login does"),
        "{report}"
    );
    running.stop().await;
}

/// Players download the patch from the public port: each file as `mh3sp patch` writes it. The game's HTTP port, the
/// Nintendo login's, serves none of it.
#[tokio::test]
async fn the_patch_downloads_from_the_public_port() {
    let (mut config, dir) = config();
    let running = server::start(&config).await.unwrap();
    for gone in ["/patch", "/errors", "/patch/codes/RMHP08.gct"] {
        assert_eq!(http_status(running.nas, gone).await, "404", "{gone}");
    }
    let (status, page) = http_get(running.public, "/patch").await;
    assert_eq!(status, "200");
    assert!(
        page.contains("href=\"/errors\""),
        "the guide links the errors"
    );
    let (status, errors) = http_get(running.public, "/errors").await;
    assert_eq!(status, "200");
    assert!(
        errors.contains("<td>11669</td>") && errors.contains("Test Server"),
        "{errors}"
    );
    let written = dir.path().join("patch");
    server::patch::write(&written, DOMAIN, &config.tls_dir).unwrap();
    let paths: Vec<&str> = page
        .split("href=\"/patch/")
        .skip(1)
        .filter_map(|r| r.split('"').next())
        .collect();
    assert_eq!(paths.len(), 6, "{page}");
    for path in paths {
        let (status, body) = http_bytes(running.public, &format!("/patch/{path}")).await;
        assert_eq!(status, "200", "{path}");
        assert_eq!(body, std::fs::read(written.join(path)).unwrap(), "{path}");
    }
    assert_eq!(
        http_status(running.public, "/patch/codes/RMHJ08.gct").await,
        "404"
    );
    running.stop().await;
    config.domain = None;
    let running = server::start(&config).await.unwrap();
    assert_eq!(
        http_status(running.public, "/patch").await,
        "404",
        "not configured"
    );
    running.stop().await;
}

#[tokio::test]
async fn refuses_everyone_when_not_configured() {
    let (mut config, _db) = config();
    config.domain = None;
    let running = server::start(&config).await.unwrap();
    let (status, text) = opn_login(running.opn, b"RMHP08", &NATNEG_HOSTNAMES).await;
    assert_eq!(status, 1);
    assert!(text.unwrap().contains("not configured"));
    assert_eq!(http_status(running.public, "/health/live").await, "200");
    assert_eq!(http_status(running.public, "/health/ready").await, "503");
    running.stop().await;
}

#[tokio::test]
async fn maintenance_mode_refuses_with_the_message() {
    let (mut config, _db) = config();
    config.maintenance = Some("Back at 18:00 UTC".into());
    let running = server::start(&config).await.unwrap();
    assert_eq!((running.lmp, running.fmp), (None, None));
    let login = opn_login(running.opn, b"RMHP08", &NATNEG_HOSTNAMES).await;
    assert_eq!(login, (1, Some("Back at 18:00 UTC".into())));
    assert_eq!(http_status(running.public, "/health/live").await, "200");
    assert_eq!(http_status(running.public, "/health/ready").await, "503");
    running.stop().await;
}

/// Decision 9: a database that doesn't open at startup is retried; until then the server is unhealthy and
/// consoles are told it isn't ready.
#[tokio::test]
async fn waits_for_the_database_at_startup() {
    let (mut config, dir) = config();
    // SQLite creates the file, not its folder: no folder, no database, until the test makes it.
    let folder = dir.path().join("later");
    config.database_url = format!("sqlite://{}", folder.join("mh3sp.db").display());
    let running = server::start(&config).await.unwrap();
    assert_eq!(http_status(running.public, "/health/live").await, "503");
    let (status, text) = opn_login(running.opn, b"RMHP08", &NATNEG_HOSTNAMES).await;
    assert_eq!(status, 1);
    assert!(text.unwrap().contains("not ready"));

    std::fs::create_dir(&folder).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while http_status(running.public, "/health/live").await != "200" {
        assert!(
            tokio::time::Instant::now() < deadline,
            "database never opened"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(http_status(running.public, "/health/ready").await, "200");
    assert_eq!(
        opn_login(running.opn, b"RMHP08", &NATNEG_HOSTNAMES).await,
        (5, None)
    );
    running.stop().await;
}

/// A PAL console's connection data: its version constant (4), its support code and its ticket, when it has them.
fn connection_data(support_code: Option<&str>, ticket: Option<&str>) -> PatData {
    let mut data = PatData::new()
        .with(0x03, Item::Long(4))
        .with(0x07, Item::String(Bytes::from_static(b"1002121503")));
    if let Some(code) = support_code {
        data = data.with(0x01, Item::String(code.to_owned().into()));
    }
    if let Some(ticket) = ticket {
        data = data.with(0x02, Item::Binary(ticket.to_owned().into()));
    }
    data
}

fn string(item: Option<&Item>) -> String {
    match item {
        Some(Item::String(s) | Item::Binary(s)) => String::from_utf8(s.to_vec()).unwrap(),
        other => panic!("not a string: {other:?}"),
    }
}

/// Where a console logs in: LMP, and the Nintendo login whose token its first visit carries.
#[derive(Clone, Copy)]
struct Lmp {
    addr: SocketAddr,
    nas: SocketAddr,
    /// The Wii every first visit comes from; none: a new one each time.
    wii: Option<u32>,
}

impl Lmp {
    fn of(running: &server::Running) -> Self {
        Lmp {
            addr: running.lmp.unwrap(),
            nas: running.nas,
            wii: None,
        }
    }
}

/// A first visit's `ReqLoginInfo` data: the token of a Nintendo login, from a Wii never seen before.
async fn login_data(nas: SocketAddr) -> PatData {
    static WIIS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let wii = WIIS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    login_data_from(nas, wii).await
}

impl Lmp {
    /// A first visit's `ReqLoginInfo` data, from this side's Wii.
    async fn login_data(self) -> PatData {
        match self.wii {
            Some(wii) => login_data_from(self.nas, wii).await,
            None => login_data(self.nas).await,
        }
    }
}

/// A first visit's `ReqLoginInfo` data, from Wii number `wii`.
async fn login_data_from(nas: SocketAddr, wii: u32) -> PatData {
    let (mac, serial) = (format!("0017ab{wii:06x}"), format!("LEH{wii:09}"));
    let token = testkit::nas::svcloc(nas, &mac, &serial).await.unwrap();
    PatData::new().with(0x0a, Item::Binary(token.into()))
}

/// LMP's first visit: the support code (issued when `None`), the ticket, and the 6 slots' Capcom IDs.
async fn first_visit(
    lmp: Lmp,
    support_code: Option<&str>,
) -> (String, String, Vec<(String, String)>) {
    let mut console = Console::connect(lmp.addr, false).await.unwrap();
    expect::<ReqConnection>(&mut console).await;
    send(
        &mut console,
        0,
        AnsConnection {
            data: connection_data(support_code, None),
        },
    )
    .await;
    assert_eq!(expect::<NtcLogin>(&mut console).await.status, 1);
    let data = lmp.login_data().await;
    send(&mut console, 1, ReqLoginInfo { data }).await;
    let code = string(expect::<AnsLoginInfo>(&mut console).await.data.get(0x07));
    send(&mut console, 2, ReqTicket {}).await;
    let ticket =
        String::from_utf8(expect::<AnsTicket>(&mut console).await.ticket.to_vec()).unwrap();
    send(&mut console, 3, ReqUserListData { first: 1, count: 6 }).await;
    let slots = expect::<AnsUserListData>(&mut console).await.users;
    send(&mut console, 4, ReqShut { kind: 2 }).await;
    expect::<AnsShut>(&mut console).await;
    (
        code,
        ticket,
        slots
            .iter()
            .map(|u| {
                (
                    string(u.get(0x02)),
                    u.get(0x03).map_or_else(String::new, |n| string(Some(n))),
                )
            })
            .collect(),
    )
}

/// LMP's second visit: plays `slot` as `name` (none: as the console does for a slot listed with its hunter's
/// name), returns its Capcom ID.
async fn second_visit(lmp: SocketAddr, ticket: &str, slot: u32, name: Option<&str>) -> String {
    let mut console = Console::connect(lmp, false).await.unwrap();
    expect::<ReqConnection>(&mut console).await;
    send(
        &mut console,
        0,
        AnsConnection {
            data: connection_data(None, Some(ticket)),
        },
    )
    .await;
    assert_eq!(expect::<NtcLogin>(&mut console).await.status, 2);
    let user = match name {
        Some(name) => PatData::new().with(0x03, Item::String(name.to_owned().into())),
        None => PatData::new(),
    };
    send(&mut console, 1, ReqUserObject { unk: 1, slot, user }).await;
    string(expect::<AnsUserObject>(&mut console).await.user.get(0x02))
}

/// The whole login of `support_code`'s console playing `slot`: the ticket FMP takes.
async fn login(lmp: Lmp, support_code: &str, slot: u32) -> String {
    let (_, ticket, _) = first_visit(lmp, Some(support_code)).await;
    let mut console = Console::connect(lmp.addr, false).await.unwrap();
    expect::<ReqConnection>(&mut console).await;
    send(
        &mut console,
        0,
        AnsConnection {
            data: connection_data(None, Some(&ticket)),
        },
    )
    .await;
    expect::<NtcLogin>(&mut console).await;
    let user = PatData::new().with(0x03, Item::String(Bytes::from_static(b"Aiko")));
    send(&mut console, 1, ReqUserObject { unk: 1, slot, user }).await;
    expect::<AnsUserObject>(&mut console).await;
    send(&mut console, 2, ReqTicket {}).await;
    let ticket = expect::<AnsTicket>(&mut console).await.ticket;
    send(&mut console, 3, ReqShut { kind: 2 }).await;
    expect::<AnsShut>(&mut console).await;
    String::from_utf8(ticket.to_vec()).unwrap()
}

async fn fmp_login(fmp: SocketAddr, ticket: &str) -> Console {
    let mut console = Console::connect(fmp, false).await.unwrap();
    expect::<ReqConnection>(&mut console).await;
    send(
        &mut console,
        0,
        AnsConnection {
            data: connection_data(None, Some(ticket)),
        },
    )
    .await;
    assert_eq!(expect::<NtcLogin>(&mut console).await.status, 3);
    console
}

/// Both FMP connections come with the ticket of the last LMP visit; a second login of the same Capcom ID kicks
/// the first.
#[tokio::test]
async fn fmp_sessions_hop_and_kick() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let (lmp, fmp) = (Lmp::of(&running), running.fmp.unwrap());
    let (code, _, _) = first_visit(lmp, None).await;
    let ticket = login(lmp, &code, 1).await;

    // Server selection, then the world server: the same ticket twice.
    let mut selection = fmp_login(fmp, &ticket).await;
    send(&mut selection, 1, ReqShut { kind: 2 }).await;
    expect::<AnsShut>(&mut selection).await;
    let mut world = fmp_login(fmp, &ticket).await;
    send(&mut world, 1, ReqServerTime { country: 0 }).await;
    expect::<AnsServerTime>(&mut world).await;

    // The same console logs in again on the same slot: the first connection is dropped.
    let again = login(lmp, &code, 1).await;
    let _second = fmp_login(fmp, &again).await;
    assert!(matches!(
        world.recv(Duration::from_secs(5)).await.unwrap(),
        Received::Closed
    ));
    running.stop().await;
}

/// Tier 3: accounts through LMP, and across a restart.
#[tokio::test]
async fn consoles_keep_their_hunters() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let lmp = Lmp::of(&running);

    // A first-time console gets a support code and empty slots; its first hunter gets a Capcom ID.
    let (code, ticket, slots) = first_visit(lmp, None).await;
    assert_eq!(code.len(), 11);
    assert!(slots.iter().all(|(id, _)| id == "******"));
    let capcom_id = second_visit(lmp.addr, &ticket, 2, Some("Aiko")).await;
    assert_eq!(capcom_id.len(), 6);

    // A redeemed ticket doesn't work twice: the connection is dropped.
    let mut reused = Console::connect(lmp.addr, false).await.unwrap();
    expect::<ReqConnection>(&mut reused).await;
    send(
        &mut reused,
        0,
        AnsConnection {
            data: connection_data(None, Some(&ticket)),
        },
    )
    .await;
    assert!(matches!(
        reused.recv(Duration::from_secs(5)).await.unwrap(),
        Received::Closed
    ));

    // Another console gets another Capcom ID.
    let (other, ticket, _) = first_visit(lmp, None).await;
    assert_ne!(other, code);
    assert_ne!(
        second_visit(lmp.addr, &ticket, 2, Some("Aiko")).await,
        capcom_id
    );
    running.stop().await;

    // After a restart, the console finds its hunter in the same slot, with the same Capcom ID.
    let running = server::start(&config).await.unwrap();
    let lmp = Lmp::of(&running);
    let (again, ticket, slots) = first_visit(lmp, Some(&code)).await;
    assert_eq!(again, code);
    assert_eq!(slots[1], (capcom_id.clone(), "Aiko".to_owned()));
    // Picking it without a name (it was listed with its hunter's) keeps the name.
    assert_eq!(second_visit(lmp.addr, &ticket, 2, None).await, capcom_id);
    let (_, _, slots) = first_visit(lmp, Some(&code)).await;
    assert_eq!(slots[1], (capcom_id, "Aiko".to_owned()));
    running.stop().await;
}

/// Tier 3, the unknown requests (review, 2026-10-11): one the game knows but the server doesn't serve is refused with
/// a message and the player stays; the few typed for a client without a known sender get their answer; one the
/// console never sends still drops the connection.
#[tokio::test]
async fn unknown_requests_are_refused_not_dropped() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let mut console = enter_world(&running).await;
    let raw = |id: u32, payload: &'static [u8]| Frame {
        seq: 10,
        id: wire::PacketId(id),
        payload: Bytes::from_static(payload),
    };
    // ReqCircleKick: refused, the request's own answer id carries the alert.
    console.send(raw(0x6535_0100, b"")).await.unwrap();
    let text = expect_alert(&mut console, wire::PacketId(0x6535_0200)).await;
    assert!(text.contains("Not available on this server"), "{text}");
    send(
        &mut console,
        11,
        ReqCircleSearchHead {
            rest: Default::default(),
        },
    )
    .await;
    let found = expect::<AnsCircleSearchHead>(&mut console).await;
    assert_eq!(found.total, 0);
    send(&mut console, 12, ReqServerTime { country: 0 }).await;
    expect::<AnsServerTime>(&mut console).await;
    // ReqTicket2: the server's to send.
    console.send(raw(0x6031_0100, b"")).await.unwrap();
    assert!(matches!(
        console.recv(Duration::from_secs(5)).await.unwrap(),
        Received::Closed
    ));
    running.stop().await;
}

/// Tier 3, rate limits (the review, 2026-10-11): a console flooding chat reaches the others with its burst only, and
/// nobody is dropped for it.
#[tokio::test]
async fn a_chat_flood_is_cut_to_its_rate() {
    let (mut config, _db) = config();
    config.rate_limits = true;
    let running = server::start(&config).await.unwrap();
    let (mut a, mut b) = (enter_world(&running).await, enter_world(&running).await);
    meet(&mut a, &mut b, 1, 1).await;
    for c in [&mut a, &mut b] {
        drain(c, Duration::from_millis(100)).await;
    }
    for seq in 0..100 {
        let line = NtcLayerChat {
            channel: 1,
            info: PatData::new(),
            text: Bytes::from_static(b"spam"),
        };
        send(&mut a, 100 + seq, line).await;
    }
    let seen = drain(&mut b, Duration::from_millis(300)).await;
    let lines = seen.iter().filter(|&&id| id == NtcLayerChat::ID).count();
    // The burst of 5, and what a slow machine refills meanwhile (1 a second): far from 100.
    assert!((5..=10).contains(&lines), "{lines} lines");
    // Both still here.
    for c in [&mut a, &mut b] {
        send(c, 300, ReqServerTime { country: 0 }).await;
        expect::<AnsServerTime>(c).await;
    }
    running.stop().await;
}

/// Chat commands (`game::commands`): `/tell` reaches its target alone; an admin command from a player answers
/// "Unknown command" to them alone; an admin's `/kick` drops its target.
#[tokio::test]
async fn chat_commands() {
    let (mut config, _db) = config();
    // The admin's Capcom ID exists once its console has played: a first server makes it, a second one names it.
    let running = server::start(&config).await.unwrap();
    let lmp = Lmp::of(&running);
    let codes = [
        first_visit(lmp, None).await.0,
        first_visit(lmp, None).await.0,
        first_visit(lmp, None).await.0,
    ];
    let mut ids = Vec::new();
    for code in &codes {
        login(lmp, code, 1).await;
        ids.push(first_visit(lmp, Some(code)).await.2[0].0.clone());
    }
    running.stop().await;
    config.admins = vec![ids[0].clone()];
    let running = server::start(&config).await.unwrap();
    let mut consoles = Vec::new();
    for code in &codes {
        let mut c = play(&running, code).await;
        let info = PatData::new();
        send(&mut c, 10, ReqLayerDown { layer: 1, info }).await;
        consoles.push(c);
    }
    let [mut admin, mut b, mut c] = consoles.try_into().ok().unwrap();
    for console in [&mut admin, &mut b, &mut c] {
        drain(console, Duration::from_millis(100)).await;
    }
    let line = |text: String| NtcLayerChat {
        channel: 1,
        info: PatData::new(),
        text: text.into(),
    };
    async fn texts(console: &mut Console) -> Vec<String> {
        let mut texts = Vec::new();
        while let Received::Frame(f) = console.recv(Duration::from_millis(200)).await.unwrap() {
            if f.id == NtcLayerChat::ID {
                let chat = NtcLayerChat::decode(&f.payload).unwrap();
                texts.push(String::from_utf8(chat.text.to_vec()).unwrap());
            }
        }
        texts
    }

    // Plain chat reaches both others: they share the place.
    send(&mut admin, 20, line("hello".into())).await;
    assert_eq!(texts(&mut b).await, ["hello"]);
    assert_eq!(texts(&mut c).await, ["hello"]);

    send(&mut admin, 21, line(format!("/tell {} psst", ids[1]))).await;
    assert_eq!(texts(&mut b).await, ["psst"]);
    assert!(texts(&mut c).await.is_empty(), "a tell heard by a third");
    assert_eq!(texts(&mut admin).await, ["To Aiko: sent."]);
    // b blocks the admin: their tells are "sent", never delivered (a block isn't revealed).
    let db = server::database::open(&config.database_url).await.unwrap();
    sqlx::query(
        "INSERT INTO blacklist (blocker, blocked) SELECT b.id, a.id FROM hunters b, hunters a
         WHERE b.capcom_id = ?1 AND a.capcom_id = ?2",
    )
    .bind(&ids[1])
    .bind(&ids[0])
    .execute(&db)
    .await
    .unwrap();
    send(&mut admin, 28, line(format!("/tell {} again", ids[1]))).await;
    assert_eq!(texts(&mut admin).await, ["To Aiko: sent."]);
    assert!(texts(&mut b).await.is_empty(), "a tell through a block");

    // Not an admin: refused as unknown, to the sender alone; nobody kicked.
    for command in [
        format!("/kick {}", ids[2]),
        format!("/ban {} x", ids[2]),
        format!("/unban {}", ids[2]),
        "/announce hi".to_owned(),
        "/stop 1h".to_owned(),
    ] {
        send(&mut b, 22, line(command.clone())).await;
        assert_eq!(texts(&mut b).await, ["Unknown command."], "{command}");
    }
    assert!(texts(&mut c).await.is_empty());
    send(&mut b, 23, line("/tell nobody hi".into())).await;
    assert_eq!(texts(&mut b).await, ["No player nobody online."]);
    send(&mut b, 24, line("/tell Aiko hi".into())).await;
    assert_eq!(
        texts(&mut b).await,
        ["Several players named Aiko:", "use a Capcom ID."]
    );

    send(&mut admin, 25, line(format!("/kick {}", ids[2]))).await;
    assert!(matches!(
        c.recv(Duration::from_secs(2)).await.unwrap(),
        Received::Closed
    ));
    assert_eq!(texts(&mut admin).await, [format!("{} kicked.", ids[2])]);
    // A ban from the chat answers once done (the database's own task); c, offline, by Capcom ID.
    send(&mut admin, 26, line(format!("/ban {} spam", ids[2]))).await;
    assert_eq!(texts(&mut admin).await, [format!("{} banned.", ids[2])]);
    send(&mut admin, 27, line(format!("/unban {}", ids[2]))).await;
    assert_eq!(texts(&mut admin).await, [format!("{} unbanned.", ids[2])]);
    // Still here: the admin and b, told c left (the admin's notice went with its answer).
    let notices = drain(&mut b, Duration::from_millis(200)).await;
    assert!(notices.contains(&NtcLayerOut::ID));
    for console in [&mut admin, &mut b] {
        send(console, 30, ReqServerTime { country: 0 }).await;
        expect::<AnsServerTime>(console).await;
    }
    running.stop().await;
}

/// What a console's chat line gets back: the answers from "[MH3SP]", read for a short while.
async fn say(console: &mut Console, seq: u16, text: &str) -> Vec<String> {
    let line = NtcLayerChat {
        channel: 1,
        info: PatData::new(),
        text: text.to_owned().into(),
    };
    send(console, seq, line).await;
    let mut texts = Vec::new();
    // The first line is waited for (a loaded host answers late); the short window is only for "no more".
    let mut window = Duration::from_secs(5);
    while let Received::Frame(f) = console.recv(window).await.unwrap() {
        if f.id == NtcLayerChat::ID {
            let chat = NtcLayerChat::decode(&f.payload).unwrap();
            texts.push(String::from_utf8(chat.text.to_vec()).unwrap());
            window = Duration::from_millis(300);
        }
    }
    texts
}

/// A server with an admin, who is in the first gate and ready to type commands. The admin is a console that has to
/// exist first, its Capcom ID being known only after its first visit: the server runs once, then again with that ID in
/// `admins` (the database stays).
async fn with_admin(config: &mut Config) -> (server::Running, Console, String) {
    let running = server::start(config).await.unwrap();
    let lmp = Lmp::of(&running);
    let (code, _, _) = first_visit(lmp, None).await;
    login(lmp, &code, 1).await;
    let id = first_visit(lmp, Some(&code)).await.2[0].0.clone();
    running.stop().await;
    config.admins = vec![id.clone()];
    let running = server::start(config).await.unwrap();
    let mut admin = play(&running, &code).await;
    let info = PatData::new();
    send(&mut admin, 10, ReqLayerDown { layer: 1, info }).await;
    drain(&mut admin, Duration::from_millis(100)).await;
    (running, admin, id)
}

/// A ban (`access.rs`), typed in the chat by an admin: the player kicked and their console refused at login with a
/// message (an alert, not a drop); lifted, it logs in again. An unknown player is refused, and the ban records the
/// admin who typed it.
#[tokio::test]
async fn a_banned_console_is_refused_until_unbanned() {
    let (mut config, _db) = config();
    let (running, mut admin, admin_id) = with_admin(&mut config).await;
    let lmp = Lmp::of(&running);
    let db = server::database::open(&config.database_url).await.unwrap();
    let (code, _, _) = first_visit(lmp, None).await;
    let mut console = play(&running, &code).await;
    let capcom_id = first_visit(lmp, Some(&code)).await.2[0].0.clone();

    assert_eq!(
        say(&mut admin, 20, "/ban NOBODY x").await,
        ["No Capcom ID NOBODY."]
    );
    // The same console again, between LMP and FMP: its ticket waits, it isn't online to be kicked.
    let ticket = login(lmp, &code, 1).await;
    assert_eq!(
        say(&mut admin, 21, &format!("/ban {capcom_id} cheating")).await,
        [format!("{capcom_id} banned.")]
    );
    // Two rules (the console, and its Wii), both by the admin who typed the command.
    let admin_row: i64 = sqlx::query_scalar("SELECT id FROM hunters WHERE capcom_id = ?1")
        .bind(&admin_id)
        .fetch_one(&db)
        .await
        .unwrap();
    let by: Vec<Option<i64>> = sqlx::query_scalar("SELECT created_by FROM access_rules")
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(by, [Some(admin_row); 2], "the ban's author");
    assert!(matches!(
        console.recv(Duration::from_secs(2)).await.unwrap(),
        Received::Closed
    ));
    let mut hop = Console::connect(running.fmp.unwrap(), false).await.unwrap();
    expect::<ReqConnection>(&mut hop).await;
    let data = connection_data(None, Some(&ticket));
    send(&mut hop, 0, AnsConnection { data }).await;
    assert!(
        matches!(
            hop.recv(Duration::from_secs(2)).await.unwrap(),
            Received::Closed
        ),
        "a ticket from before the ban"
    );
    match login_info(lmp, Some(&code)).await {
        Some(Received::Frame(f)) => assert_eq!(f.id.0, AnsLoginInfo::ID.0 | 0x01, "an alert"),
        other => panic!("expected an alert, got {other:?}"),
    }

    let unban = format!("/unban {}", capcom_id.to_lowercase());
    assert_eq!(
        say(&mut admin, 22, &unban).await,
        [format!("{capcom_id} unbanned.")]
    );
    assert_eq!(
        say(&mut admin, 23, &unban).await,
        [format!("{capcom_id} isn't banned.")],
        "twice"
    );
    match login_info(lmp, Some(&code)).await {
        Some(Received::Frame(f)) => assert_eq!(f.id, AnsLoginInfo::ID),
        other => panic!("expected a login, got {other:?}"),
    }
    running.stop().await;
}

/// A ban is lifted whole (mh3-rs/server#61): a banned console that tried from another Wii in the meantime must not
/// have taken that Wii over, or `/unban` (which finds the Wii through the console's current one) left the ban on the
/// first, and the console was refused from it for good.
#[tokio::test]
async fn an_unban_lifts_the_ban_of_a_console_that_tried_another_wii() {
    let (mut config, _db) = config();
    let (running, mut admin, _) = with_admin(&mut config).await;
    let first = Lmp {
        wii: Some(900_001),
        ..Lmp::of(&running)
    };
    let second = Lmp {
        wii: Some(900_002),
        ..first
    };
    let (code, _, _) = first_visit(first, None).await;
    let _console = play(&running, &code).await;
    let capcom_id = first_visit(first, Some(&code)).await.2[0].0.clone();
    assert_eq!(
        say(&mut admin, 20, &format!("/ban {capcom_id} cheating")).await,
        [format!("{capcom_id} banned.")]
    );
    // From the first Wii and from another: refused both times.
    for lmp in [first, second] {
        match login_info(lmp, Some(&code)).await {
            Some(Received::Frame(f)) => assert_eq!(f.id.0, AnsLoginInfo::ID.0 | 0x01, "an alert"),
            other => panic!("expected an alert, got {other:?}"),
        }
    }
    assert_eq!(
        say(&mut admin, 21, &format!("/unban {capcom_id}")).await,
        [format!("{capcom_id} unbanned.")]
    );
    // Both Wiis log in again, the first as well as the one it tried.
    for lmp in [first, second, first] {
        match login_info(lmp, Some(&code)).await {
            Some(Received::Frame(f)) => assert_eq!(f.id, AnsLoginInfo::ID),
            other => panic!("expected a login, got {other:?}"),
        }
    }
    running.stop().await;
}

/// The relayed traffic's limit (`game::fmp`, burst 40, 20 a second): a flood of positions is cut near the burst, and
/// traffic at the game's own peak rate afterwards all goes through.
#[tokio::test]
async fn relayed_traffic_is_cut_to_its_rate() {
    let (mut config, _db) = config();
    config.rate_limits = true;
    let running = server::start(&config).await.unwrap();
    let (mut a, mut b) = (enter_world(&running).await, enter_world(&running).await);
    meet(&mut a, &mut b, 1, 1).await;
    for c in [&mut a, &mut b] {
        drain(c, Duration::from_millis(100)).await;
    }
    let position = || NtcLayerUserPosition {
        data: wire::packets::Rest(position_lp2(&[0; 37]).into()),
    };
    let positions = |ids: Vec<wire::PacketId>| {
        ids.iter()
            .filter(|&&id| id == NtcLayerUserPosition::ID)
            .count()
    };
    for seq in 0..200 {
        send(&mut a, 100 + seq, position()).await;
    }
    let flood = positions(drain(&mut b, Duration::from_millis(300)).await);
    // The burst, and what a slow machine refills meanwhile: far from 200.
    assert!((40..=55).contains(&flood), "{flood} positions");
    // Refilled after two seconds: the game's peak (9 in a second) passes whole.
    tokio::time::sleep(Duration::from_secs(2)).await;
    for seq in 0..9 {
        send(&mut a, 400 + seq, position()).await;
    }
    assert_eq!(
        positions(drain(&mut b, Duration::from_millis(300)).await),
        9
    );
    running.stop().await;
}

/// LMP's first visit up to `ReqLoginInfo`: the answer, or `None` when the server closed the connection.
async fn login_info(lmp: Lmp, support_code: Option<&str>) -> Option<Received> {
    let mut console = Console::connect(lmp.addr, false).await.unwrap();
    expect::<ReqConnection>(&mut console).await;
    let data = connection_data(support_code, None);
    send(&mut console, 0, AnsConnection { data }).await;
    if let Received::Closed = console.recv(Duration::from_secs(5)).await.unwrap() {
        return None;
    }
    let info = ReqLoginInfo {
        data: lmp.login_data().await,
    };
    send(&mut console, 1, info).await;
    match console.recv(Duration::from_secs(5)).await.unwrap() {
        Received::Closed => None,
        other => Some(other),
    }
}

/// Tier 3, the per-address limits' wiring (the review, 2026-10-11), with loopback's exemption off: LMP's first
/// visit only after OPN; five new consoles an address, a made-up support code counting as one; 16 connections.
#[tokio::test]
async fn the_per_address_limits_hold() {
    let (mut config, _db) = config();
    config.loopback_exempt = false;
    let running = server::start(&config).await.unwrap();
    let (opn, lmp) = (running.opn, Lmp::of(&running));
    assert!(login_info(lmp, None).await.is_none(), "LMP before OPN");
    let (mut console, status) = opn_connect(opn, b"RMHP08", &NATNEG_HOSTNAMES).await;
    assert_eq!(status, 5);
    send(&mut console, 1, ReqLmpConnect {}).await;
    expect::<AnsLmpConnect>(&mut console).await;
    drop(console);

    let mut codes = Vec::new();
    for _ in 0..5 {
        codes.push(first_visit(lmp, None).await.0);
    }
    assert!(login_info(lmp, None).await.is_none(), "a sixth console");
    assert!(
        login_info(lmp, Some("ZZZZZZZZZZZ")).await.is_none(),
        "a made-up code"
    );
    // A known console still comes in.
    assert_eq!(first_visit(lmp, Some(&codes[0])).await.0, codes[0]);

    let mut open = Vec::new();
    for _ in 0..16 {
        let mut c = Console::connect(lmp.addr, false).await.unwrap();
        expect::<ReqConnection>(&mut c).await;
        open.push(c);
    }
    let mut seventeenth = Console::connect(lmp.addr, false).await.unwrap();
    assert!(matches!(
        seventeenth.recv(Duration::from_secs(5)).await.unwrap(),
        Received::Closed
    ));
    running.stop().await;
}

/// Tier 3, ADR 0009 at LMP: a database failure at login is refused with a message, and the console asking again
/// keeps its own support code (it isn't handed a new one).
#[tokio::test]
async fn a_database_failure_at_login_keeps_the_code() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let lmp = Lmp::of(&running);
    let (code, _, _) = first_visit(lmp, None).await;
    let db = sqlx::SqlitePool::connect(&config.database_url)
        .await
        .unwrap();
    let sql = |sql: &'static str| sqlx::query(sql).execute(&db);
    let mut console = Console::connect(lmp.addr, false).await.unwrap();
    expect::<ReqConnection>(&mut console).await;
    let data = connection_data(Some(&code), None);
    send(&mut console, 0, AnsConnection { data }).await;
    assert_eq!(expect::<NtcLogin>(&mut console).await.status, 1);
    sql("ALTER TABLE consoles RENAME TO hidden").await.unwrap();
    // The token is used up by the first ask: the second, without it, is the same Wii.
    let data = login_data(lmp.nas).await;
    send(&mut console, 1, ReqLoginInfo { data }).await;
    let text = expect_alert(&mut console, AnsLoginInfo::ID).await;
    assert!(text.contains("Not available right now"), "{text}");
    sql("ALTER TABLE hidden RENAME TO consoles").await.unwrap();
    let data = PatData::new();
    send(&mut console, 2, ReqLoginInfo { data }).await;
    let again = expect::<AnsLoginInfo>(&mut console).await;
    assert_eq!(string(again.data.get(0x07)), code);
    running.stop().await;
}

/// Tier 3, rate limits: quest joins beyond their burst are refused with "Too fast", each one checked before.
#[tokio::test]
async fn quest_joins_are_rate_limited() {
    let (mut config, _db) = config();
    config.rate_limits = true;
    let running = server::start(&config).await.unwrap();
    let (mut a, mut b) = (enter_world(&running).await, enter_world(&running).await);
    for step in 1..=2 {
        meet(&mut a, &mut b, 1, step).await;
    }
    drain(&mut a, Duration::from_millis(100)).await;
    let mut texts = Vec::new();
    for seq in 0..6 {
        let join = ReqCircleJoin {
            circle: 99,
            info: PatData::new(),
        };
        send(&mut a, 100 + seq, join).await;
        texts.push(expect_alert(&mut a, AnsCircleJoin::ID).await);
    }
    assert!(
        texts[..5].iter().all(|t| !t.contains("Too fast")),
        "{texts:?}"
    );
    assert!(texts[5].contains("Too fast"), "{texts:?}");
    running.stop().await;
}

/// A malformed packet drops the connection.
#[tokio::test]
async fn malformed_packets_drop_the_connection() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let mut console = Console::connect(running.lmp.unwrap(), false).await.unwrap();
    expect::<ReqConnection>(&mut console).await;
    let garbage = Frame {
        seq: 0,
        id: AnsConnection::ID,
        payload: Bytes::from_static(b"\x05\x01"),
    };
    console.send(garbage).await.unwrap();
    assert!(matches!(
        console.recv(Duration::from_secs(5)).await.unwrap(),
        Received::Closed
    ));
    running.stop().await;
}

/// A new console's whole way in: login, server selection (Valor1), then the world server connection.
async fn enter_world(running: &server::Running) -> Console {
    let (code, _, _) = first_visit(Lmp::of(running), None).await;
    play(running, &code).await
}

/// A new game of `support_code`'s console, slot 1, up to a world server.
async fn play(running: &server::Running, support_code: &str) -> Console {
    let (lmp, fmp) = (Lmp::of(running), running.fmp.unwrap());
    let ticket = login(lmp, support_code, 1).await;
    let mut selection = fmp_login(fmp, &ticket).await;
    send(
        &mut selection,
        1,
        ReqFmpInfo2 {
            index: 1,
            fields: vec![2, 3],
        },
    )
    .await;
    expect::<AnsFmpInfo2>(&mut selection).await;
    send(&mut selection, 2, ReqShut { kind: 2 }).await;
    expect::<AnsShut>(&mut selection).await;
    fmp_login(fmp, &ticket).await
}

/// The records of a layer list (gates or cities), `count` from `first`.
async fn children(
    console: &mut Console,
    seq: u16,
    first: u32,
    count: u32,
) -> Vec<(PatData, wire::OptionalFields)> {
    send(console, seq, ReqLayerChildListData { first, count }).await;
    expect::<AnsLayerChildListData>(console).await.children
}

fn long(item: Option<&Item>) -> u32 {
    match item {
        Some(Item::Long(v)) => *v,
        other => panic!("not a long: {other:?}"),
    }
}

/// The next frame: an alert answering `answer` (its id with kind byte 0x01), and its text.
async fn expect_alert(console: &mut Console, answer: wire::PacketId) -> String {
    match console.recv(Duration::from_secs(5)).await.unwrap() {
        Received::Frame(f) if f.id.0 == answer.0 | 0x01 => {
            let mut r = wire::Reader::new(&f.payload);
            assert_eq!(r.u32().unwrap(), 1);
            String::from_utf8(r.lp2_string().unwrap().to_vec()).unwrap()
        }
        other => panic!("expected an alert for {answer}, got {other:?}"),
    }
}

/// A city is created only by the player in its gate who holds its number (the sprint "A world with no impossible
/// state", mh3-rs/server#57): a second reservation replaces the first, so the first number can't be created, and
/// once in a city (with a quest) no other can. A console that sends it anyway is refused and the server keeps
/// serving: the sequence used to leave the console in a quest of a city it had left, and its next request panicked
/// under the world's lock, which stops the process.
#[tokio::test]
async fn a_city_is_created_only_from_the_reservation_held() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let mut c = enter_world(&running).await;
    let info = PatData::new();
    send(&mut c, 10, ReqLayerDown { layer: 1, info }).await;
    expect::<AnsLayerDown>(&mut c).await;
    let set = |number| ReqLayerCreateSet {
        number,
        layer: PatData::new(),
        extra: Default::default(),
    };
    // Two numbers reserved one after the other: only the second is held.
    for number in [1, 2] {
        send(&mut c, 11, ReqLayerCreateHead { number }).await;
        expect::<AnsLayerCreateHead>(&mut c).await;
    }
    send(&mut c, 12, set(1)).await;
    let refused = expect_alert(&mut c, AnsLayerCreateSet::ID).await;
    assert!(refused.contains("no longer exists"), "{refused}");
    send(&mut c, 13, set(2)).await;
    expect::<AnsLayerCreateSet>(&mut c).await;
    send(&mut c, 14, ReqLayerCreateFoot { number: 2, unk: 0 }).await;
    expect::<AnsLayerCreateFoot>(&mut c).await;
    let info = PatData::new().with(0x09, Item::Long(4));
    let extra = Default::default();
    send(&mut c, 15, ReqCircleCreate { info, extra }).await;
    expect::<NtcCircleListLayerChange>(&mut c).await;
    assert_eq!(expect::<AnsCircleCreate>(&mut c).await.circle, 1);
    // In a city with a quest: a city that doesn't exist, and the one it is in, are both refused.
    send(&mut c, 16, set(1)).await;
    let refused = expect_alert(&mut c, AnsLayerCreateSet::ID).await;
    assert!(refused.contains("no longer exists"), "{refused}");
    send(&mut c, 17, set(2)).await;
    let refused = expect_alert(&mut c, AnsLayerCreateSet::ID).await;
    assert!(refused.contains("already exists"), "{refused}");
    // Still in its quest: its next request is answered, and the world holds together.
    let options = PatData::new().with(0x03, Item::Byte(1));
    send(&mut c, 18, ReqCircleMatchOptionSet { options }).await;
    expect::<AnsCircleMatchOptionSet>(&mut c).await;
    assert_eq!(running.violations(), Vec::<String>::new());
    running.stop().await;
}

/// A reservation ends with the player's stay in the gate (mh3-rs/server#67): one who reserved a city and went back up
/// leaves it free, where it used to show as full until the player left a city or disconnected.
#[tokio::test]
async fn a_reserved_city_is_free_again_once_its_creator_goes_up() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let mut a = enter_world(&running).await;
    let mut b = enter_world(&running).await;
    for c in [&mut a, &mut b] {
        let info = PatData::new();
        send(c, 10, ReqLayerDown { layer: 1, info }).await;
        expect::<AnsLayerDown>(c).await;
    }
    // A sees B arrive.
    drain(&mut a, Duration::from_millis(200)).await;
    send(&mut a, 11, ReqLayerCreateHead { number: 1 }).await;
    expect::<AnsLayerCreateHead>(&mut a).await;
    drain(&mut b, Duration::from_millis(200)).await;
    let cities = children(&mut b, 11, 1, 1).await;
    assert_eq!(cities[0].0.get(0x10), Some(&Item::Byte(2)), "reserved");
    send(&mut a, 12, ReqLayerUp {}).await;
    expect::<AnsLayerUp>(&mut a).await;
    drain(&mut b, Duration::from_millis(200)).await;
    let cities = children(&mut b, 12, 1, 1).await;
    assert_eq!(cities[0].0.get(0x10), Some(&Item::Byte(1)), "free again");
    // And another can take the number.
    send(&mut b, 13, ReqLayerCreateHead { number: 1 }).await;
    expect::<AnsLayerCreateHead>(&mut b).await;
    assert_eq!(running.violations(), Vec::<String>::new());
    running.stop().await;
}

/// Tier 3, solo: a gate, a city, a quest, as one console sees them.
#[tokio::test]
async fn solo_city_and_quest() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let mut c = enter_world(&running).await;

    // Gate 1: the gate list counts the player there; its player list has the hunter.
    send(
        &mut c,
        10,
        ReqLayerDown {
            layer: 1,
            info: PatData::new(),
        },
    )
    .await;
    expect::<AnsLayerDown>(&mut c).await;
    send(&mut c, 11, ReqLayerUp {}).await;
    expect::<AnsLayerUp>(&mut c).await;
    let gates = children(&mut c, 12, 1, 40).await;
    assert_eq!(gates.len(), 40);
    // Back at the server level: nobody in gate 1 any more (state 1, empty).
    assert_eq!(long(gates[0].0.get(0x06)), 0);
    send(
        &mut c,
        13,
        ReqLayerDown {
            layer: 1,
            info: PatData::new(),
        },
    )
    .await;
    expect::<AnsLayerDown>(&mut c).await;
    send(&mut c, 14, ReqLayerUserList { fields: vec![] }).await;
    let users = expect::<AnsLayerUserList>(&mut c).await.users;
    assert_eq!(users.len(), 1);
    assert_eq!(string(users[0].get(0x02)), "Aiko");

    // City 3: reserved (shown full) between head and foot, then joinable with the player in it.
    send(&mut c, 15, ReqLayerCreateHead { number: 3 }).await;
    expect::<AnsLayerCreateHead>(&mut c).await;
    let settings = wire::OptionalFields(vec![(1, Some(0xffff_ffff)), (4, Some(0))]);
    send(
        &mut c,
        16,
        ReqLayerCreateSet {
            number: 3,
            layer: PatData::new(),
            extra: settings.clone(),
        },
    )
    .await;
    expect::<AnsLayerCreateSet>(&mut c).await;
    send(&mut c, 17, ReqLayerCreateFoot { number: 3, unk: 0 }).await;
    expect::<AnsLayerCreateFoot>(&mut c).await;
    // Asked about its own layer (0), the console gets 0 back, so it takes the record as its own (Local Info): one
    // player of 4, and the city's settings.
    let own = ReqLayerChildInfo {
        layer: 0,
        fields: vec![],
        rest: wire::packets::Rest(Bytes::new()),
    };
    send(&mut c, 40, own).await;
    let info = expect::<AnsLayerChildInfo>(&mut c).await;
    assert_eq!(
        (
            info.layer,
            long(info.record.get(0x06)),
            long(info.record.get(0x09))
        ),
        (0, 1, 4)
    );
    assert_eq!(info.settings, settings);

    // A quest: the creator gets the city's notification before its answer.
    let info = PatData::new()
        .with(0x09, Item::Long(4))
        .with(0x0e, Item::Byte(1));
    send(
        &mut c,
        18,
        ReqCircleCreate {
            info,
            extra: wire::OptionalFields(vec![(1, Some(4)), (2, Some(10000))]),
        },
    )
    .await;
    let change = expect::<NtcCircleListLayerChange>(&mut c).await;
    assert_eq!(change.circle, 1);
    assert_eq!(change.extra.0, vec![(1, Some(4)), (2, Some(10000))]);
    assert_eq!(expect::<AnsCircleCreate>(&mut c).await.circle, 1);
    send(
        &mut c,
        19,
        ReqCircleMatchOptionSet {
            options: PatData::new().with(0x03, Item::Byte(1)),
        },
    )
    .await;
    expect::<AnsCircleMatchOptionSet>(&mut c).await;
    send(&mut c, 20, ReqCircleMatchStart {}).await;
    let start = expect::<NtcCircleMatchStart>(&mut c).await;
    // One player departs: count 1, then index 1 and the Capcom ID.
    assert_eq!(&start.data[..5], &[0, 0, 0, 1, 1]);
    expect::<AnsCircleMatchStart>(&mut c).await;
    send(&mut c, 21, ReqCircleLeave { circle: 1 }).await;
    let gone = expect::<NtcCircleListLayerChange>(&mut c).await;
    assert_eq!(gone.extra.0, vec![(1, Some(4)), (2, Some(0))]);
    expect::<AnsCircleLeave>(&mut c).await;

    // Back in the gate: city 3 went with its last player (no population, no settings left).
    send(&mut c, 22, ReqLayerUp {}).await;
    expect::<AnsLayerUp>(&mut c).await;
    let cities = children(&mut c, 23, 1, 10).await;
    assert_eq!(long(cities[2].0.get(0x06)), 0);
    assert!(cities[2].1.0.is_empty(), "city 3 still has its settings");
    send(&mut c, 24, ReqLayerEnd {}).await;
    expect::<AnsLayerEnd>(&mut c).await;
    running.stop().await;
}

/// A path a PAL console can jump to (M5 D11): its version constant 4, world server, 1, gate, city.
fn jump_path(server: u32, gate: u16, city: u16) -> Bytes {
    let mut path = Vec::new();
    path.put_u32(4);
    path.put_u32(server);
    path.put_u16(1);
    path.put_u16(gate);
    path.put_u16(city);
    path.into()
}

/// The Capcom ID of a console alone where it stands.
async fn own_id(console: &mut Console, seq: u16) -> String {
    send(console, seq, ReqLayerUserList { fields: vec![] }).await;
    string(expect::<AnsLayerUserList>(console).await.users[0].get(0x01))
}

/// Tier 3, jumps within a world server (M5 D10–D12): Change Gates' rows and a found player's card carry paths the
/// console can jump to; a jump into a city is an arrival there; a city that can't take the player refuses.
#[tokio::test]
async fn jumps_within_a_world_server() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    // B creates City 1 in City Gate 2; A stands in City Gate 1.
    let mut b = enter_world(&running).await;
    send(
        &mut b,
        10,
        ReqLayerDown {
            layer: 2,
            info: PatData::new(),
        },
    )
    .await;
    expect::<AnsLayerDown>(&mut b).await;
    send(&mut b, 11, ReqLayerCreateHead { number: 1 }).await;
    expect::<AnsLayerCreateHead>(&mut b).await;
    let set = ReqLayerCreateSet {
        number: 1,
        layer: PatData::new(),
        extra: Default::default(),
    };
    send(&mut b, 12, set).await;
    expect::<AnsLayerCreateSet>(&mut b).await;
    send(&mut b, 13, ReqLayerCreateFoot { number: 1, unk: 0 }).await;
    expect::<AnsLayerCreateFoot>(&mut b).await;
    let b_id = own_id(&mut b, 14).await;
    let mut a = enter_world(&running).await;
    send(
        &mut a,
        10,
        ReqLayerDown {
            layer: 1,
            info: PatData::new(),
        },
    )
    .await;
    expect::<AnsLayerDown>(&mut a).await;

    // Change Gates: the world server's 40 gates, each with the path a pick jumps to.
    let head = ReqLayerSiblingListHead {
        first: 1,
        count: 40,
        fields: vec![],
    };
    send(&mut a, 11, head).await;
    assert_eq!(expect::<AnsLayerSiblingListHead>(&mut a).await.count, 40);
    send(
        &mut a,
        12,
        ReqLayerSiblingListData {
            first: 1,
            count: 10,
        },
    )
    .await;
    let gates = expect::<AnsLayerSiblingListData>(&mut a).await.siblings;
    assert_eq!(gates.len(), 10);
    assert_eq!(
        gates[1].0.get(0x02),
        Some(&Item::Binary(jump_path(1, 2, 0)))
    );

    // B's card: where "Warp to" goes, B's city.
    let card = ReqUserSearchInfo {
        capcom_id: b_id.clone().into(),
        info: PatData::new(),
    };
    send(&mut a, 13, card).await;
    let to_b = expect::<AnsUserSearchInfo>(&mut a).await.info;
    assert_eq!(to_b.get(0x04), Some(&Item::Binary(jump_path(1, 2, 1))));

    // A jumps there: B's city now has 2 of 4.
    let jump = ReqLayerJump {
        path: jump_path(1, 2, 1),
        match_key: 0,
    };
    send(&mut a, 14, jump).await;
    expect::<AnsLayerJump>(&mut a).await;
    let counts = expect::<NtcLayerUserNum>(&mut b).await;
    assert_eq!((counts.kind, long(counts.layer.get(0x02))), (2, 2));

    // A city that doesn't exist: refused, and A stays where it is.
    let jump = ReqLayerJump {
        path: jump_path(1, 2, 3),
        match_key: 0,
    };
    send(&mut a, 15, jump).await;
    assert!(
        expect_alert(&mut a, AnsLayerJump::ID)
            .await
            .contains("no longer exists")
    );
    send(&mut a, 16, ReqLayerUserList { fields: vec![] }).await;
    assert_eq!(expect::<AnsLayerUserList>(&mut a).await.users.len(), 2);
    running.stop().await;
}

/// City search isn't built (M5 D10): a console asking finds no city instead of being dropped (M5 D15).
#[tokio::test]
async fn city_search_finds_nothing() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let mut console = enter_world(&running).await;
    let search = Rest(Bytes::from_static(&[
        1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 10, 0, 0,
    ]));
    send(&mut console, 10, ReqLayerDetailSearchHead { search }).await;
    assert_eq!(
        expect::<AnsLayerDetailSearchHead>(&mut console).await.count,
        0
    );
    send(
        &mut console,
        11,
        ReqLayerDetailSearchData { first: 1, count: 1 },
    )
    .await;
    assert_eq!(
        expect::<AnsLayerDetailSearchData>(&mut console).await.count,
        0
    );
    let rest = Rest(Bytes::new());
    send(&mut console, 12, ReqLayerDetailSearchFoot { rest }).await;
    expect::<AnsLayerDetailSearchFoot>(&mut console).await;
    running.stop().await;
}

/// Tier 3, a jump to another world server (M5 D13): a ticket, the server selection's handshake, then the ticket
/// places the player there, once.
#[tokio::test]
async fn jumps_to_another_world_server() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let (lmp, fmp) = (Lmp::of(&running), running.fmp.unwrap());
    let (code, _, _) = first_visit(lmp, None).await;
    let ticket = login(lmp, &code, 1).await;
    let mut selection = fmp_login(fmp, &ticket).await;
    let pick = |index| ReqFmpInfo2 {
        index,
        fields: vec![2, 3],
    };
    send(&mut selection, 1, pick(1)).await;
    expect::<AnsFmpInfo2>(&mut selection).await;
    send(&mut selection, 2, ReqShut { kind: 2 }).await;
    expect::<AnsShut>(&mut selection).await;
    let mut a = fmp_login(fmp, &ticket).await;

    // To City Gate 3 of world server 2.
    let ready = ReqLayerJumpReady {
        path: jump_path(2, 3, 0),
        match_key: 0,
    };
    send(&mut a, 10, ready).await;
    let jump = expect::<NtcLayerJumpReady>(&mut a).await.ticket;
    send(&mut a, 11, pick(2)).await;
    expect::<AnsFmpInfo2>(&mut a).await;
    send(&mut a, 12, ReqLayerEnd {}).await;
    expect::<AnsLayerEnd>(&mut a).await;
    send(&mut a, 13, ReqShut { kind: 2 }).await;
    expect::<AnsShut>(&mut a).await;
    let mut a = fmp_login(fmp, &ticket).await;
    let go = || ReqLayerJumpGo {
        path: jump_path(2, 3, 0),
        ticket: jump,
    };
    send(&mut a, 10, go()).await;
    expect::<NtcLayerJumpGo>(&mut a).await;
    let a_id = own_id(&mut a, 11).await;
    let card = ReqUserSearchInfo {
        capcom_id: a_id.into(),
        info: PatData::new(),
    };
    send(&mut a, 12, card).await;
    let here = expect::<AnsUserSearchInfo>(&mut a).await.info;
    assert_eq!(here.get(0x04), Some(&Item::Binary(jump_path(2, 3, 0))));

    // The ticket is spent.
    send(&mut a, 13, go()).await;
    assert!(
        expect_alert(&mut a, NtcLayerJumpGo::ID)
            .await
            .contains("can't go there")
    );
    running.stop().await;
}

/// A visit to the server selection places nobody (mh3-rs/server#74): the world server a session picked is spent by
/// the visit that entered it, so one that left the world and came back to the selection isn't counted on it.
#[tokio::test]
async fn a_selection_visit_after_play_places_nobody() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let (lmp, fmp) = (Lmp::of(&running), running.fmp.unwrap());
    // Another console at the server selection, which counts the players of world server 1 when asked.
    let (other, _, _) = first_visit(lmp, None).await;
    let other_ticket = login(lmp, &other, 1).await;
    let mut observer = fmp_login(fmp, &other_ticket).await;
    let mut population = async |seq| {
        let list = ReqFmpListData2 { first: 1, count: 1 };
        send(&mut observer, seq, list).await;
        long(expect::<AnsFmpListData2>(&mut observer).await.servers[0].get(0x08))
    };
    assert_eq!(population(1).await, 0);

    let (code, _, _) = first_visit(lmp, None).await;
    let ticket = login(lmp, &code, 1).await;
    let mut selection = fmp_login(fmp, &ticket).await;
    let pick = ReqFmpInfo2 {
        index: 1,
        fields: vec![2, 3],
    };
    send(&mut selection, 1, pick).await;
    expect::<AnsFmpInfo2>(&mut selection).await;
    send(&mut selection, 2, ReqShut { kind: 2 }).await;
    expect::<AnsShut>(&mut selection).await;
    // On world server 1: counted there.
    let mut play = fmp_login(fmp, &ticket).await;
    assert_eq!(population(2).await, 1, "on world server 1");
    // Out of the world and back at the selection, on the same ticket: nobody is on world server 1.
    send(&mut play, 11, ReqLayerEnd {}).await;
    expect::<AnsLayerEnd>(&mut play).await;
    send(&mut play, 12, ReqShut { kind: 2 }).await;
    expect::<AnsShut>(&mut play).await;
    let _back = fmp_login(fmp, &ticket).await;
    assert_eq!(population(3).await, 0, "back at the selection");
    assert_eq!(running.violations(), Vec::<String>::new());
    running.stop().await;
}

/// Tier 3, two consoles in a gate: a city taken is refused, then joined; leadership passes on.
#[tokio::test]
async fn cities_are_shared() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let mut a = enter_world(&running).await;
    let mut b = enter_world(&running).await;
    for (c, seq) in [(&mut a, 10), (&mut b, 10)] {
        send(
            c,
            seq,
            ReqLayerDown {
                layer: 2,
                info: PatData::new(),
            },
        )
        .await;
        expect::<AnsLayerDown>(c).await;
    }
    // A, already in the gate, sees B arrive.
    let arrived = expect::<NtcLayerIn>(&mut a).await;
    assert_eq!(string(arrived.user.get(0x02)), "Aiko");
    send(&mut a, 11, ReqLayerCreateHead { number: 1 }).await;
    expect::<AnsLayerCreateHead>(&mut a).await;
    // Reserved: shown full to the other player, at once (its gate's list stays live, as MH3SP) and when it asks;
    // it can't create the city.
    let info = expect::<NtcLayerInfoSet>(&mut b).await;
    assert_eq!(info.record.get(0x10), Some(&Item::Byte(2)));
    let cities = children(&mut b, 11, 1, 1).await;
    assert!(matches!(cities[0].0.get(0x10), Some(Item::Byte(2))));
    send(&mut b, 12, ReqLayerCreateHead { number: 1 }).await;
    assert!(
        expect_alert(&mut b, AnsLayerCreateHead::ID)
            .await
            .contains("already exists")
    );
    send(
        &mut a,
        12,
        ReqLayerCreateSet {
            number: 1,
            layer: PatData::new(),
            extra: Default::default(),
        },
    )
    .await;
    expect::<AnsLayerCreateSet>(&mut a).await;
    let counts = expect::<NtcLayerUserNum>(&mut b).await;
    assert_eq!((counts.kind, long(counts.layer.get(0x02))), (4, 1));
    send(&mut a, 13, ReqLayerCreateFoot { number: 1, unk: 0 }).await;
    expect::<AnsLayerCreateFoot>(&mut a).await;
    let info = expect::<NtcLayerInfoSet>(&mut b).await;
    assert_eq!(info.record.get(0x10), Some(&Item::Byte(3)));

    // Created: joinable (state 3) with one player; the other joins it.
    let cities = children(&mut b, 13, 1, 1).await;
    assert_eq!(
        (long(cities[0].0.get(0x06)), cities[0].0.get(0x10)),
        (1, Some(&Item::Byte(3)))
    );
    send(
        &mut b,
        14,
        ReqLayerDown {
            layer: 1,
            info: PatData::new(),
        },
    )
    .await;
    expect::<AnsLayerDown>(&mut b).await;
    send(&mut b, 15, ReqLayerUserList { fields: vec![] }).await;
    assert_eq!(expect::<AnsLayerUserList>(&mut b).await.users.len(), 2);

    // A learns the city's new population (what Local Info shows): 2 of 4.
    let counts = |ntc: NtcLayerUserNum| {
        (
            ntc.kind,
            long(ntc.layer.get(0x02)),
            long(ntc.layer.get(0x05)),
        )
    };
    assert_eq!(counts(expect::<NtcLayerUserNum>(&mut a).await), (2, 2, 4));

    // The creator leaves the game: B sees them go and becomes the city's host; the city stays, with 1 of 4.
    send(&mut a, 14, ReqLayerUserList { fields: vec![] }).await;
    let a_id = string(expect::<AnsLayerUserList>(&mut a).await.users[0].get(0x01));
    drop(a);
    assert_eq!(
        String::from_utf8(expect::<NtcLayerOut>(&mut b).await.capcom_id.to_vec()).unwrap(),
        a_id
    );
    assert_eq!(counts(expect::<NtcLayerUserNum>(&mut b).await), (2, 1, 4));
    let host = expect::<NtcLayerHost>(&mut b).await;
    assert_ne!(String::from_utf8(host.capcom_id.to_vec()).unwrap(), a_id);
    send(&mut b, 16, ReqLayerUserList { fields: vec![] }).await;
    assert_eq!(expect::<AnsLayerUserList>(&mut b).await.users.len(), 1);
    running.stop().await;
}

/// A new console with its hunter on slot 1: its support code and Capcom ID.
async fn new_hunter(lmp: Lmp) -> (String, String) {
    let (code, _, _) = first_visit(lmp, None).await;
    login(lmp, &code, 1).await;
    let (_, _, slots) = first_visit(lmp, Some(&code)).await;
    (code, slots[0].0.clone())
}

/// The message notices (`NtcTell`, `NtcFriendAccept`, `NtcFriendAdd`) a console receives within 200 ms: what a test
/// that expects none uses (silence can only be waited for).
async fn mail(console: &mut Console) -> Vec<wire::PacketId> {
    let kinds = [NtcTell::ID, NtcFriendAccept::ID, NtcFriendAdd::ID];
    let seen = drain(console, Duration::from_millis(200)).await;
    seen.into_iter().filter(|id| kinds.contains(id)).collect()
}

/// The message notices a console receives when some are due: it waits (up to 5 s) for the first, then 200 ms for the
/// rest. Not `mail`: under load a notice comes later than that window, and a test that expects one must not fail
/// for it.
async fn delivered(console: &mut Console) -> Vec<wire::PacketId> {
    let kinds = [NtcTell::ID, NtcFriendAccept::ID, NtcFriendAdd::ID];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut seen = Vec::new();
    while seen.is_empty() {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match console
            .recv(left.max(Duration::from_millis(1)))
            .await
            .unwrap()
        {
            Received::Frame(f) if kinds.contains(&f.id) => seen.push(f.id),
            Received::Frame(_) => {}
            _ => return seen,
        }
    }
    seen.extend(mail(console).await);
    seen
}

/// Tier 3: messages, with or without a friend request, are stored and delivered once, when the recipient stands in
/// a city (the game's inbox drops what arrives while a scene loads, and forgets it all when the game quits).
#[tokio::test]
async fn messages_wait_for_their_recipient() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let lmp = Lmp::of(&running);
    let (a_code, a_id) = new_hunter(lmp).await;
    let (b_code, b_id) = new_hunter(lmp).await;
    let (c_code, c_id) = new_hunter(lmp).await;
    let tell = |to: &str, text: &'static [u8]| ReqTell {
        capcom_id: to.to_owned().into(),
        info: PatData::new(),
        text: Bytes::from_static(text),
    };
    let request = |to: &str| ReqFriendAdd {
        capcom_id: to.to_owned().into(),
        info: PatData::new(),
        text: Bytes::from_static(b"friends?"),
    };
    let position = || NtcLayerUserPosition {
        data: wire::packets::Rest(position_lp2(&[0; 37]).into()),
    };

    // While B is away: a message and a friend request from A.
    let mut a = play(&running, &a_code).await;
    send(&mut a, 10, tell(&b_id, b"hello")).await;
    send(&mut a, 11, request(&b_id)).await;
    drain(&mut a, Duration::from_millis(100)).await;

    // B: nothing in a gate, nor on entering a city; at its first position there, both, once.
    let mut b = play(&running, &b_code).await;
    let info = PatData::new();
    send(&mut b, 10, ReqLayerDown { layer: 1, info }).await;
    meet(&mut b, &mut enter_world(&running).await, 1, 2).await;
    assert_eq!(mail(&mut b).await, []);
    send(&mut b, 30, position()).await;
    assert_eq!(delivered(&mut b).await, [NtcTell::ID, NtcFriendAccept::ID]);
    send(&mut b, 31, position()).await;
    assert_eq!(mail(&mut b).await, []);

    // B stands in the city now: a message arrives at once; a blocked hunter's never does.
    let mut c = play(&running, &c_code).await;
    send(&mut c, 10, tell(&b_id, b"hi")).await;
    assert_eq!(delivered(&mut b).await, [NtcTell::ID]);
    let block = ReqBlackAdd {
        capcom_id: c_id.into(),
        info: PatData::new(),
    };
    send(&mut b, 32, block).await;
    drain(&mut b, Duration::from_millis(100)).await;
    send(&mut c, 11, tell(&b_id, b"blocked")).await;
    assert_eq!(mail(&mut b).await, []);

    // A asks again: delivered again. B asks A too: friends at once, both told.
    send(&mut a, 12, request(&b_id)).await;
    assert_eq!(delivered(&mut b).await, [NtcFriendAccept::ID]);
    send(&mut b, 33, request(&a_id)).await;
    assert_eq!(delivered(&mut b).await, [NtcFriendAdd::ID]);
    assert_eq!(delivered(&mut a).await, [NtcFriendAdd::ID]);
    running.stop().await;
}

/// Tier 3, ADR 0009: the database failing mid-game refuses the request with a message and keeps the
/// player; what doesn't need the database goes on.
#[tokio::test]
async fn a_database_failure_keeps_the_player() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let (code, me) = new_hunter(Lmp::of(&running)).await;
    let mut console = play(&running, &code).await;
    // A table gone under the server, as a corrupted file would have it; back afterwards.
    let db = sqlx::SqlitePool::connect(&config.database_url)
        .await
        .unwrap();
    let sql = |sql: &'static str| sqlx::query(sql).execute(&db);
    sql("ALTER TABLE blacklist RENAME TO hidden").await.unwrap();
    let list = ReqBlackList {
        first: 1,
        count: 10,
        fields: vec![],
    };
    send(&mut console, 10, list).await;
    let text = expect_alert(&mut console, AnsBlackList::ID).await;
    assert!(text.contains("Not available right now"), "{text}");
    send(&mut console, 11, ReqServerTime { country: 0 }).await;
    expect::<AnsServerTime>(&mut console).await;
    sql("ALTER TABLE hidden RENAME TO blacklist").await.unwrap();

    // Mail that can't be read at a position in a city waits for the next one.
    let (other_code, _) = new_hunter(Lmp::of(&running)).await;
    let mut other = play(&running, &other_code).await;
    for step in 1..=3 {
        meet(&mut console, &mut other, 1, step).await;
    }
    let tell = ReqTell {
        capcom_id: me.into(),
        info: PatData::new(),
        text: Bytes::from_static(b"hello"),
    };
    send(&mut other, 30, tell).await;
    // Stored before the table goes: its answer is the proof (other frames may come first).
    loop {
        match other.recv(Duration::from_secs(5)).await.unwrap() {
            Received::Frame(f) if f.id == AnsTell::ID => break,
            Received::Frame(_) => {}
            gone => panic!("no answer to the tell: {gone:?}"),
        }
    }
    let position = || NtcLayerUserPosition {
        data: wire::packets::Rest(position_lp2(&[0; 37]).into()),
    };
    sql("ALTER TABLE messages RENAME TO hidden").await.unwrap();
    send(&mut console, 31, position()).await;
    assert_eq!(mail(&mut console).await, []);
    sql("ALTER TABLE hidden RENAME TO messages").await.unwrap();
    send(&mut console, 32, position()).await;
    assert_eq!(delivered(&mut console).await, [NtcTell::ID]);
    running.stop().await;
}

/// Tier 3: a profile notice for one player (`target` 1, naming them) reaches only them; one for everyone (2), all.
#[tokio::test]
async fn profile_notices_reach_their_target() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let lmp = Lmp::of(&running);
    let mut consoles = Vec::new();
    let mut ids = Vec::new();
    for _ in 0..3 {
        let (code, id) = new_hunter(lmp).await;
        let mut c = play(&running, &code).await;
        let info = PatData::new();
        send(&mut c, 10, ReqLayerDown { layer: 1, info }).await;
        consoles.push(c);
        ids.push(id);
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    for c in &mut consoles {
        drain(c, Duration::from_millis(100)).await;
    }
    let notice = |target: u8, to: &str| ReqUserBinaryNotice {
        target,
        capcom_id: to.to_owned().into(),
        offset: 0,
        size: 2,
    };
    let got = |ids: Vec<wire::PacketId>| ids.contains(&NtcUserBinaryNotice::ID);
    send(&mut consoles[0], 20, notice(1, &ids[1])).await;
    let (b, c) = (
        drain(&mut consoles[1], Duration::from_millis(200)).await,
        drain(&mut consoles[2], Duration::from_millis(200)).await,
    );
    assert_eq!(
        (got(b), got(c)),
        (true, false),
        "target 1: only the named player"
    );
    send(&mut consoles[0], 21, notice(2, "")).await;
    let (b, c) = (
        drain(&mut consoles[1], Duration::from_millis(200)).await,
        drain(&mut consoles[2], Duration::from_millis(200)).await,
    );
    assert_eq!(
        (got(b), got(c)),
        (true, true),
        "target 2: everyone at the place"
    );
    running.stop().await;
}

/// Tier 3: `mh3sp announce` reaches every logged-in player, wherever they are, from "[MH3SP]".
#[cfg(target_os = "linux")]
#[tokio::test]
async fn announcements_reach_everyone() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let mut in_world = enter_world(&running).await;
    let mut in_gate = enter_world(&running).await;
    let info = PatData::new();
    send(&mut in_gate, 10, ReqLayerDown { layer: 1, info }).await;
    drain(&mut in_gate, Duration::from_millis(100)).await;
    let control = config.control.clone();
    let name = control.clone();
    let answer = tokio::task::spawn_blocking(move || {
        server::control::announce(&name, "Maintenance tonight")
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(answer, "sent to 2 players");
    for c in [&mut in_world, &mut in_gate] {
        let chat = expect::<NtcLayerChat>(c).await;
        assert_eq!(string(chat.info.get(0x04)), "[MH3SP]");
        assert_eq!(&chat.text[..], b"Maintenance tonight");
    }
    let empty = tokio::task::spawn_blocking(move || server::control::announce(&control, " "))
        .await
        .unwrap();
    assert!(empty.is_err());
    running.stop().await;
}

/// Tier 3: `mh3sp stop` warns players, refuses nothing meanwhile, reminds them, can be cancelled, and stops the server
/// at its end.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn stopping_warns_and_can_be_cancelled() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let stop = |delay: &'static str| {
        let control = config.control.clone();
        tokio::task::spawn_blocking(move || server::control::stop(&control, delay))
    };
    let mut c = enter_world(&running).await;
    let said = |chat: NtcLayerChat| String::from_utf8(chat.text.to_vec()).unwrap();

    assert_eq!(stop("6s").await.unwrap().unwrap(), "stopping in 6 seconds");
    assert_eq!(
        said(expect::<NtcLayerChat>(&mut c).await),
        "Server stopping in 6 seconds."
    );
    // Nothing refused meanwhile: a new login goes through.
    assert_eq!(
        opn_login(running.opn, b"RMHE08", &NATNEG_HOSTNAMES).await.0,
        5
    );
    // The 5 s reminder, then cancelled.
    let reminder = tokio::time::timeout(Duration::from_secs(3), expect::<NtcLayerChat>(&mut c))
        .await
        .unwrap();
    assert_eq!(said(reminder), "Server stopping in 5 seconds.");
    assert_eq!(stop("cancel").await.unwrap().unwrap(), "stop cancelled");
    assert_eq!(
        said(expect::<NtcLayerChat>(&mut c).await),
        "Server stop cancelled."
    );
    assert!(stop("cancel").await.unwrap().is_err(), "nothing to cancel");
    assert!(stop("soon").await.unwrap().is_err(), "not a delay");

    // Carried out: the server stops.
    stop("1s").await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(3), running.stopping().cancelled())
        .await
        .unwrap();
    running.stop().await;
}

/// Tier 3: `/metrics` counts players per world server, NAT negotiations and requests (never a Capcom ID).
#[tokio::test]
async fn metrics_count_what_happens() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let _first = enter_world(&running).await;
    let _second = enter_world(&running).await;
    negotiate(&running, 7, false).await;
    let (status, body) = http_get(running.public, "/metrics").await;
    assert_eq!(status, "200");
    let m: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(m["players"], 2);
    // The release running: every crate of the workspace has the same version.
    assert_eq!(m["version"], env!("CARGO_PKG_VERSION"), "{body}");
    assert_eq!(m["world_servers"]["Valor1"], 2);
    assert_eq!(m["natneg"]["pairs_direct"], 1);
    assert!(m["requests"]["count"].as_u64().unwrap() > 10, "{body}");
    assert!(!body.contains("capcom"), "{body}");
    // The tests build the server without the `prometheus` feature: no such endpoint.
    assert_eq!(
        http_status(running.public, "/metrics/prometheus").await,
        "404"
    );
    running.stop().await;
}

/// Tier 3, M5 runs 15–16: players in a hunt get announcements as party chat from themselves (town chat doesn't show
/// there, party chat only from a member), others as town chat; party chat a member sends reaches the others.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn hunters_get_announcements() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let (mut a, mut b) = party(&running, 1, true).await;
    let mut in_world = enter_world(&running).await;
    let control = config.control.clone();
    tokio::task::spawn_blocking(move || server::control::announce(&control, "Hello"))
        .await
        .unwrap()
        .unwrap();
    for hunter in [&mut a, &mut b] {
        let seen = drain(hunter, Duration::from_millis(200)).await;
        assert!(
            seen.contains(&NtcCircleChat::ID) && !seen.contains(&NtcLayerChat::ID),
            "{seen:?}"
        );
    }
    expect::<NtcLayerChat>(&mut in_world).await;

    // Party chat: to the other member, the sender filled in.
    let chat = NtcCircleChat {
        info: PatData::new(),
        text: Bytes::from_static(b"hi"),
    };
    send(&mut a, 40, chat).await;
    let got = expect::<NtcCircleChat>(&mut b).await;
    assert_eq!(
        (&got.text[..], string(got.info.get(0x04))),
        (&b"hi"[..], "Aiko".to_owned())
    );

    running.stop().await;
}

/// Tier 3: city seats (M5): one player per seat, the others told when it's taken and freed; quitting while seated
/// frees it.
#[tokio::test]
async fn one_player_per_seat() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let mut a = enter_world(&running).await;
    let mut b = enter_world(&running).await;
    for step in 1..=3 {
        meet(&mut a, &mut b, 1, step).await;
    }
    drain(&mut a, Duration::from_millis(200)).await;
    drain(&mut b, Duration::from_millis(200)).await;
    let sit = |stand: bool| PatData::new().with(0x03, Item::Byte(u8::from(!stand)));

    send(
        &mut a,
        60,
        ReqLayerMediationLock {
            slot: 3,
            items: sit(false),
        },
    )
    .await;
    assert_eq!(expect::<AnsLayerMediationLock>(&mut a).await.result, 1);
    assert_eq!(expect::<NtcLayerMediationLock>(&mut b).await.slot, 3);
    send(
        &mut b,
        60,
        ReqLayerMediationLock {
            slot: 3,
            items: sit(false),
        },
    )
    .await;
    assert!(
        expect_alert(&mut b, AnsLayerMediationLock::ID)
            .await
            .contains("already")
    );
    send(
        &mut b,
        61,
        ReqLayerMediationList {
            mode: 1,
            max_count: 32,
            fields: vec![1, 2, 3],
        },
    )
    .await;
    let seats = expect::<AnsLayerMediationList>(&mut b).await.items.0;
    assert_eq!(seats.len(), 1);
    assert_eq!(seats[0].get(0x02), Some(&Item::Byte(3)));

    send(
        &mut a,
        61,
        ReqLayerMediationUnlock {
            slot: 3,
            items: sit(true),
        },
    )
    .await;
    expect::<AnsLayerMediationUnlock>(&mut a).await;
    assert_eq!(expect::<NtcLayerMediationUnlock>(&mut b).await.slot, 3);

    // A sits again and quits: the seat is free for B.
    send(
        &mut a,
        62,
        ReqLayerMediationLock {
            slot: 5,
            items: sit(false),
        },
    )
    .await;
    expect::<AnsLayerMediationLock>(&mut a).await;
    expect::<NtcLayerMediationLock>(&mut b).await;
    drop(a);
    let seen = drain(&mut b, Duration::from_millis(300)).await;
    assert!(seen.contains(&NtcLayerMediationUnlock::ID), "{seen:?}");
    send(
        &mut b,
        62,
        ReqLayerMediationLock {
            slot: 5,
            items: sit(false),
        },
    )
    .await;
    assert_eq!(expect::<AnsLayerMediationLock>(&mut b).await.result, 1);
    running.stop().await;
}

/// Tier 3: Player Search (M5): part of a name in any case, or part of a Capcom ID, on every world server; page by page.
#[tokio::test]
async fn player_search() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let lmp = Lmp::of(&running);
    let mut searcher = enter_world(&running).await;
    let mut ids = Vec::new();
    let mut others = Vec::new();
    for _ in 0..2 {
        let (code, id) = new_hunter(lmp).await;
        others.push(play(&running, &code).await);
        ids.push(id);
    }
    let head = |capcom_id: &str, name: &'static [u8]| ReqUserSearchHead {
        capcom_id: capcom_id.to_owned().into(),
        name: Bytes::from_static(name),
        search: Default::default(),
        first: 1,
        count: 100,
        fields: vec![1, 2, 3, 4],
    };
    // Everyone is named "Aiko": part of it, another case; the searcher isn't among them.
    send(&mut searcher, 70, head("", b"aIK")).await;
    assert_eq!(expect::<AnsUserSearchHead>(&mut searcher).await.count, 2);
    send(&mut searcher, 71, ReqUserSearchData { first: 1, count: 1 }).await;
    assert_eq!(
        expect::<AnsUserSearchData>(&mut searcher).await.users.len(),
        1
    );
    send(&mut searcher, 72, ReqUserSearchData { first: 2, count: 2 }).await;
    assert_eq!(
        expect::<AnsUserSearchData>(&mut searcher).await.users.len(),
        1
    );
    // Part of a Capcom ID.
    send(&mut searcher, 73, head(&ids[0][1..], b"")).await;
    assert_eq!(expect::<AnsUserSearchHead>(&mut searcher).await.count, 1);
    send(
        &mut searcher,
        74,
        ReqUserSearchData {
            first: 1,
            count: 10,
        },
    )
    .await;
    let found = expect::<AnsUserSearchData>(&mut searcher).await.users;
    assert_eq!(string(found[0].0.get(0x01)), ids[0]);
    // Neither: nobody.
    send(&mut searcher, 75, head("", b"")).await;
    assert_eq!(expect::<AnsUserSearchHead>(&mut searcher).await.count, 0);
    send(&mut searcher, 76, ReqUserSearchFoot {}).await;
    expect::<AnsUserSearchFoot>(&mut searcher).await;
    running.stop().await;
}

/// World servers keep to their type's HR (the descriptions): at FMP from the session's profile, at LMP's first pick
/// (before any profile) from the one stored last time; a hunter never online here passes.
#[tokio::test]
async fn world_servers_keep_to_their_hr_limits() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let lmp = Lmp::of(&running);
    let (code, _, _) = first_visit(lmp, None).await;
    let code = code.as_str();
    let (beginners, veterans) = (5, 7);
    // Never online: no HR known, Beginners1 lets it in.
    let lmp_pick = move |index| async move {
        let ticket = login(lmp, code, 1).await;
        let mut console = Console::connect(lmp.addr, false).await.unwrap();
        expect::<ReqConnection>(&mut console).await;
        let data = connection_data(None, Some(&ticket));
        send(&mut console, 0, AnsConnection { data }).await;
        expect::<NtcLogin>(&mut console).await;
        send(
            &mut console,
            1,
            ReqUserObject {
                unk: 1,
                slot: 1,
                user: PatData::new(),
            },
        )
        .await;
        expect::<AnsUserObject>(&mut console).await;
        send(
            &mut console,
            2,
            ReqFmpInfo {
                index,
                fields: vec![2, 3],
            },
        )
        .await;
        match console.recv(Duration::from_secs(5)).await.unwrap() {
            Received::Frame(f) if f.id == AnsFmpInfo::ID => None,
            Received::Frame(f) if f.id.0 == AnsFmpInfo::ID.0 | 0x01 => {
                Some(String::from_utf8_lossy(&f.payload).into_owned())
            }
            other => panic!("expected AnsFmpInfo or its alert, got {other:?}"),
        }
    };
    assert_eq!(lmp_pick(beginners).await, None, "HR unknown");

    // HR 50, at FMP: Beginners refused, Veterans taken.
    let mut a = play(&running, code).await;
    send(
        &mut a,
        10,
        ReqUserBinarySet {
            offset: 0,
            data: vec![0, 50].into(),
        },
    )
    .await;
    expect::<AnsUserBinarySet>(&mut a).await;
    let pick = |index| ReqFmpInfo2 {
        index,
        fields: vec![2, 3],
    };
    send(&mut a, 11, pick(beginners)).await;
    assert!(
        expect_alert(&mut a, AnsFmpInfo2::ID)
            .await
            .contains("HR 30 or lower")
    );
    send(&mut a, 12, pick(veterans)).await;
    expect::<AnsFmpInfo2>(&mut a).await;
    drop(a);

    // The next session's first pick, at LMP: HR 50 as stored.
    let refused = lmp_pick(beginners).await.expect("refused at LMP");
    assert!(refused.contains("HR 30 or lower"), "{refused}");
    assert_eq!(lmp_pick(veterans).await, None);
    running.stop().await;
}

/// Tier 3: profile writes are validated; an invalid one is refused and others keep seeing the previous block.
#[tokio::test]
async fn invalid_profiles_are_refused() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let mut a = enter_world(&running).await;
    let mut b = enter_world(&running).await;
    // HR 5; then HR 1000.
    for (seq, offset, data) in [(10, 0, vec![0, 5]), (11, 0, vec![0x03, 0xe8])] {
        send(
            &mut a,
            seq,
            ReqUserBinarySet {
                offset,
                data: data.into(),
            },
        )
        .await;
        expect::<AnsUserBinarySet>(&mut a).await;
    }
    for (c, seq) in [(&mut a, 12), (&mut b, 10)] {
        send(
            c,
            seq,
            ReqLayerDown {
                layer: 2,
                info: PatData::new(),
            },
        )
        .await;
        expect::<AnsLayerDown>(c).await;
    }
    send(&mut b, 11, ReqLayerUserList { fields: vec![] }).await;
    let users = expect::<AnsLayerUserList>(&mut b).await.users;
    let blocks: Vec<_> = users
        .iter()
        .filter_map(|u| match u.get(0x07) {
            Some(Item::Binary(block)) => Some(block[..6].to_vec()),
            _ => None,
        })
        .collect();
    assert!(blocks.contains(&vec![0, 5, 0, 0, 0, 1]), "{blocks:?}");
    running.stop().await;
}

/// Tier 3: every content download is consistent (version, head, chunks), quests included.
#[tokio::test]
async fn content_downloads() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let mut c = enter_world(&running).await;
    for kind in 0x01..=0x0f {
        send(&mut c, 1, ReqBinaryVersion { kind }).await;
        let version = expect::<AnsBinaryVersion>(&mut c).await.version;
        send(&mut c, 2, ReqBinaryHead { kind }).await;
        let head = expect::<AnsBinaryHead>(&mut c).await;
        assert_eq!(head.version, version, "type {kind:#x}");
        // In two chunks, as consoles do for large ones.
        let half = head.size / 2;
        let mut bytes = Vec::new();
        for (offset, size) in [(0, half), (half, head.size - half)] {
            send(
                &mut c,
                3,
                ReqBinaryData {
                    kind,
                    version,
                    offset,
                    size,
                },
            )
            .await;
            let data = expect::<AnsBinaryData>(&mut c).await;
            assert_eq!(
                (data.version, data.offset, data.chunk.len() as u32),
                (version, offset, size)
            );
            bytes.extend_from_slice(&data.chunk);
        }
        send(&mut c, 4, ReqBinaryFoot { kind }).await;
        expect::<AnsBinaryFoot>(&mut c).await;
        assert_eq!(bytes.len() as u32, head.size);
    }
    // A type the server doesn't know drops the connection.
    send(&mut c, 5, ReqBinaryHead { kind: 0x7f }).await;
    assert!(matches!(
        c.recv(Duration::from_secs(5)).await.unwrap(),
        Received::Closed
    ));
    running.stop().await;
}

/// Tier 3: positions relayed through one city of 4 players reach the others in under 10 ms (99th percentile, on one
/// host). MH3SP took ~50–80 ms; the console sends positions every 500 ms, so the relay must add next to nothing.
#[tokio::test]
async fn relay_latency() {
    use std::time::Instant;
    const ROUNDS: u32 = 50;
    // One rule everywhere (review, 2026-10-11): a shared CI runner and a debug build once crossed 2 ms (pipeline 6);
    // 10 ms is still far under MH3SP's 50–80 ms, so a real regression shows.
    let p99_limit = Duration::from_millis(10);

    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let mut consoles = Vec::new();
    for _ in 0..4 {
        let mut c = enter_world(&running).await;
        send(
            &mut c,
            10,
            ReqLayerDown {
                layer: 1,
                info: PatData::new(),
            },
        )
        .await;
        expect::<AnsLayerDown>(&mut c).await;
        consoles.push(c);
    }
    send(&mut consoles[0], 11, ReqLayerCreateHead { number: 1 }).await;
    send(
        &mut consoles[0],
        12,
        ReqLayerCreateSet {
            number: 1,
            layer: PatData::new(),
            extra: Default::default(),
        },
    )
    .await;
    send(
        &mut consoles[0],
        13,
        ReqLayerCreateFoot { number: 1, unk: 0 },
    )
    .await;
    for c in consoles.iter_mut().skip(1) {
        send(
            c,
            11,
            ReqLayerDown {
                layer: 1,
                info: PatData::new(),
            },
        )
        .await;
    }
    // Let the answers and arrival notices land, then forget them.
    tokio::time::sleep(Duration::from_millis(300)).await;
    for c in consoles.iter_mut() {
        while let Received::Frame(_) = c.recv(Duration::from_millis(50)).await.unwrap() {}
    }

    let mut samples = Vec::new();
    for round in 0..ROUNDS {
        for sender in 0..consoles.len() {
            // A position as the console sends it (an lp2 of 37 bytes), the round and sender in its first bytes.
            let mut position = vec![0u8; 37];
            let mut at = &mut position[..];
            at.put_u32(round);
            at.put_u8(sender as u8);
            let mut data = vec![0, 37];
            data.extend_from_slice(&position);
            let sent = Instant::now();
            send(
                &mut consoles[sender],
                100,
                NtcLayerUserPosition {
                    data: wire::packets::Rest(data.into()),
                },
            )
            .await;
            for (receiver, c) in consoles.iter_mut().enumerate() {
                if receiver == sender {
                    continue;
                }
                let got = expect::<NtcLayerUserPosition>(c).await;
                samples.push(sent.elapsed());
                // Sender's Capcom ID (lp2, 6 bytes) in front, then the position as sent.
                assert_eq!(
                    &got.data.0[8..],
                    &position_lp2(&position)[..],
                    "receiver {receiver} got another position"
                );
            }
        }
    }
    samples.sort();
    let pct = |p: usize| samples[(samples.len() * p / 100).min(samples.len() - 1)];
    println!(
        "relay latency over {} deliveries: median {:?}, p99 {:?}, max {:?}",
        samples.len(),
        pct(50),
        pct(99),
        samples[samples.len() - 1]
    );
    assert!(pct(99) < p99_limit, "p99 {:?} over {p99_limit:?}", pct(99));
    running.stop().await;
}

fn position_lp2(position: &[u8]) -> Vec<u8> {
    let mut v = vec![0, position.len() as u8];
    v.extend_from_slice(position);
    v
}

/// What a console receives within `window`, by packet id.
async fn drain(console: &mut Console, window: Duration) -> Vec<wire::PacketId> {
    let mut ids = Vec::new();
    while let Received::Frame(f) = console.recv(window).await.unwrap() {
        ids.push(f.id);
    }
    ids
}

/// Two consoles in city 1 of `gate`, B in A's quest (A ready and B ready when `depart`, then the quest started).
async fn party(running: &server::Running, gate: u16, depart: bool) -> (Console, Console) {
    let mut a = enter_world(running).await;
    let mut b = enter_world(running).await;
    for c in [&mut a, &mut b] {
        send(
            c,
            10,
            ReqLayerDown {
                layer: gate,
                info: PatData::new(),
            },
        )
        .await;
    }
    send(&mut a, 11, ReqLayerCreateHead { number: 1 }).await;
    send(
        &mut a,
        12,
        ReqLayerCreateSet {
            number: 1,
            layer: PatData::new(),
            extra: Default::default(),
        },
    )
    .await;
    send(&mut a, 13, ReqLayerCreateFoot { number: 1, unk: 0 }).await;
    // A's city exists (its reservation over) before B enters it.
    tokio::time::sleep(Duration::from_millis(200)).await;
    send(
        &mut b,
        11,
        ReqLayerDown {
            layer: 1,
            info: PatData::new(),
        },
    )
    .await;
    let info = PatData::new().with(0x09, Item::Long(4));
    send(
        &mut a,
        14,
        ReqCircleCreate {
            info,
            extra: Default::default(),
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    send(
        &mut b,
        12,
        ReqCircleJoin {
            circle: 1,
            info: PatData::new(),
        },
    )
    .await;
    if depart {
        for c in [&mut a, &mut b] {
            send(
                c,
                20,
                ReqCircleMatchOptionSet {
                    options: PatData::new().with(0x03, Item::Byte(1)),
                },
            )
            .await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        send(&mut a, 21, ReqCircleMatchStart {}).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    drain(&mut a, Duration::from_millis(100)).await;
    drain(&mut b, Duration::from_millis(100)).await;
    (a, b)
}

/// A console says whether it is ready to depart, and the answer comes back.
async fn ready(console: &mut Console, seq: u16, ready: bool) {
    let options = PatData::new().with(0x03, Item::Byte(u8::from(ready)));
    send(console, seq, ReqCircleMatchOptionSet { options }).await;
    expect::<AnsCircleMatchOptionSet>(console).await;
}

/// A quest is departed by a leader who is ready (mh3-rs/server#65): one whose leader isn't is refused as a member's
/// departure is, with the party left as it was, where it used to be sent away from its own quest.
#[tokio::test]
async fn a_leader_not_ready_cannot_depart() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let (mut a, mut b) = party(&running, 1, false).await;
    ready(&mut b, 30, true).await;
    drain(&mut a, Duration::from_millis(200)).await;
    send(&mut a, 31, ReqCircleMatchStart {}).await;
    expect::<AnsCircleMatchStart>(&mut a).await;
    let seen = drain(&mut b, Duration::from_millis(300)).await;
    assert_eq!(seen, [], "B is told nothing: the quest didn't depart");
    // Still its quest: once ready, the leader departs it.
    ready(&mut a, 32, true).await;
    drain(&mut b, Duration::from_millis(200)).await;
    send(&mut a, 33, ReqCircleMatchStart {}).await;
    let seen = drain(&mut b, Duration::from_millis(300)).await;
    assert!(seen.contains(&NtcCircleMatchStart::ID), "{seen:?}");
    assert_eq!(running.violations(), Vec::<String>::new());
    running.stop().await;
}

/// Party data comes from a member of the quest it names (mh3-rs/server#66): a console that is in the city but not in
/// the quest reaches nobody with it, and stays connected; a member's still reaches the party.
#[tokio::test]
async fn party_data_from_outside_the_quest_reaches_nobody() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    // A leads quest 1 of city 1, B is in it; C walks into the city and not into the quest.
    let (mut a, mut b) = party(&running, 1, false).await;
    let mut c = enter_world(&running).await;
    for seq in [10, 11] {
        let info = PatData::new();
        send(&mut c, seq, ReqLayerDown { layer: 1, info }).await;
        expect::<AnsLayerDown>(&mut c).await;
    }
    drain(&mut c, Duration::from_millis(200)).await;
    drain(&mut a, Duration::from_millis(200)).await;
    drain(&mut b, Duration::from_millis(200)).await;
    send(&mut c, 12, ReqLayerUserList { fields: vec![] }).await;
    let ids: Vec<Bytes> = expect::<AnsLayerUserList>(&mut c)
        .await
        .users
        .iter()
        .map(|user| match user.get(0x01) {
            Some(Item::String(id)) => id.clone(),
            other => panic!("no Capcom ID: {other:?}"),
        })
        .collect();
    assert_eq!(ids.len(), 3);
    let data = || wire::packets::Rest(vec![0; 8].into());
    send(
        &mut c,
        13,
        NtcCircleBinary {
            circle: 1,
            data: data(),
        },
    )
    .await;
    for capcom_id in &ids {
        let packet = NtcCircleBinary2 {
            circle: 1,
            capcom_id: capcom_id.clone(),
            data: data(),
        };
        send(&mut c, 14, packet).await;
    }
    for (who, console) in [("A", &mut a), ("B", &mut b)] {
        let seen = drain(console, Duration::from_millis(300)).await;
        assert!(
            !seen.contains(&NtcCircleBinary::ID) && !seen.contains(&NtcCircleBinary2::ID),
            "{who} got party data from a player outside its quest: {seen:?}"
        );
    }
    // A member sends to everyone in the city, the outsider included: the outsider, who isn't in the quest, gets none.
    for capcom_id in &ids {
        let packet = NtcCircleBinary2 {
            circle: 1,
            capcom_id: capcom_id.clone(),
            data: data(),
        };
        send(&mut b, 17, packet).await;
    }
    let seen = drain(&mut c, Duration::from_millis(300)).await;
    assert!(
        !seen.contains(&NtcCircleBinary2::ID),
        "a recipient outside the quest got party data: {seen:?}"
    );
    // Still connected, and a member's data still reaches the party.
    send(&mut c, 15, ReqLayerUserList { fields: vec![] }).await;
    expect::<AnsLayerUserList>(&mut c).await;
    send(
        &mut b,
        16,
        NtcCircleBinary {
            circle: 1,
            data: data(),
        },
    )
    .await;
    let seen = drain(&mut a, Duration::from_millis(300)).await;
    assert!(seen.contains(&NtcCircleBinary::ID), "{seen:?}");
    assert_eq!(running.violations(), Vec::<String>::new());
    running.stop().await;
}

/// A member who isn't ready stays behind (mh3-rs/server#71): when the quest departs, it is sent away and the leader is
/// told it left, so every member of a departed quest went on it.
#[tokio::test]
async fn a_member_not_ready_is_sent_away_when_the_quest_departs() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let (mut a, mut b) = party(&running, 1, false).await;
    ready(&mut a, 30, true).await;
    drain(&mut b, Duration::from_millis(200)).await;
    send(&mut a, 31, ReqCircleMatchStart {}).await;
    let sent_away = drain(&mut b, Duration::from_millis(300)).await;
    assert!(sent_away.contains(&NtcCircleKick::ID), "{sent_away:?}");
    let told = drain(&mut a, Duration::from_millis(300)).await;
    assert!(told.contains(&NtcCircleMatchStart::ID), "{told:?}");
    assert!(told.contains(&NtcCircleLeave::ID), "B's leaving: {told:?}");
    // It is out of the quest: its party data reaches nobody.
    let data = wire::packets::Rest(vec![0; 8].into());
    send(&mut b, 32, NtcCircleBinary { circle: 1, data }).await;
    let seen = drain(&mut a, Duration::from_millis(300)).await;
    assert!(!seen.contains(&NtcCircleBinary::ID), "{seen:?}");
    assert_eq!(running.violations(), Vec::<String>::new());
    running.stop().await;
}

/// A quest departs once (mh3-rs/server#65): asked again, nothing is sent to its members; and a member's readiness
/// after departure changes nothing but is still passed on.
#[tokio::test]
async fn a_quest_departs_once() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let (mut a, mut b) = party(&running, 1, true).await;
    send(&mut a, 40, ReqCircleMatchStart {}).await;
    expect::<AnsCircleMatchStart>(&mut a).await;
    let seen = drain(&mut b, Duration::from_millis(300)).await;
    assert_eq!(seen, [], "B is told of no second departure");
    ready(&mut b, 41, false).await;
    let seen = drain(&mut a, Duration::from_millis(300)).await;
    assert!(seen.contains(&NtcCircleMatchOptionSet::ID), "{seen:?}");
    assert_eq!(running.violations(), Vec::<String>::new());
    running.stop().await;
}

/// The step `step` of two consoles meeting, A leading: 1 the gate, 2 A's city, 3 B in it, 4 A's quest, 5 B in it,
/// 6 both ready, 7 departure (0, entering the world, is done by the caller).
async fn meet(a: &mut Console, b: &mut Console, gate: u16, step: u16) {
    let seq = 10 + step;
    match step {
        1 => {
            for c in [&mut *a, &mut *b] {
                let info = PatData::new();
                send(c, seq, ReqLayerDown { layer: gate, info }).await;
            }
        }
        2 => {
            send(a, 20, ReqLayerCreateHead { number: 1 }).await;
            let set = ReqLayerCreateSet {
                number: 1,
                layer: PatData::new(),
                extra: Default::default(),
            };
            send(a, 21, set).await;
            send(a, 22, ReqLayerCreateFoot { number: 1, unk: 0 }).await;
        }
        3 => {
            let info = PatData::new();
            send(b, seq, ReqLayerDown { layer: 1, info }).await;
        }
        4 => {
            let info = PatData::new().with(0x09, Item::Long(4));
            let extra = Default::default();
            send(a, seq, ReqCircleCreate { info, extra }).await;
        }
        5 => {
            let info = PatData::new();
            send(b, seq, ReqCircleJoin { circle: 1, info }).await;
        }
        6 => {
            for c in [&mut *a, &mut *b] {
                let options = PatData::new().with(0x03, Item::Byte(1));
                send(c, seq, ReqCircleMatchOptionSet { options }).await;
            }
        }
        7 => send(a, seq, ReqCircleMatchStart {}).await,
        _ => unreachable!(),
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
}

/// Tier 3: either console vanishing at any step of meeting leaves nothing behind. The other keeps playing; once
/// both are gone, a newcomer finds the gate and city empty and creates the same city and quest.
#[tokio::test]
async fn a_disconnect_at_every_step_leaves_no_trace() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    // Each case on its own gate, all at once (logins are slow: a pause before each connection's first frame).
    let cases = (0..=7).flat_map(|steps| [(steps, true), (steps, false)]);
    let runs = cases
        .zip(1..)
        .map(|((steps, a_vanishes), gate)| vanish(&running, gate, steps, a_vanishes));
    futures_util::future::join_all(runs).await;
    running.stop().await;
}

async fn vanish(running: &server::Running, gate: u16, steps: u16, a_vanishes: bool) {
    let at = format!(
        "{} vanishing after step {steps}",
        if a_vanishes { "A" } else { "B" }
    );
    let mut a = enter_world(running).await;
    let mut b = enter_world(running).await;
    for step in 1..=steps {
        meet(&mut a, &mut b, gate, step).await;
    }
    let (gone, mut stays) = if a_vanishes { (a, b) } else { (b, a) };
    drop(gone);
    let seen = drain(&mut stays, Duration::from_millis(200)).await;
    // Seen go: from the city once both are in it (step 3), from the quest once both are in it (step 5).
    for (from, expected) in [(3, NtcLayerOut::ID), (5, NtcCircleLeave::ID)] {
        if steps >= from {
            assert!(seen.contains(&expected), "{at}: no {expected} in {seen:?}");
        }
    }
    if steps > 0 {
        send(&mut stays, 40, ReqLayerUserList { fields: vec![] }).await;
        let users = expect::<AnsLayerUserList>(&mut stays).await.users;
        assert_eq!(users.len(), 1, "{at}: the one left sees others");
    }
    drop(stays);
    tokio::time::sleep(Duration::from_millis(100)).await;

    nothing_left(running, gate, &at).await;
}

/// Nobody left in `gate`, nor in its city 1: a newcomer creates that city and its quest 1.
async fn nothing_left(running: &server::Running, gate: u16, at: &str) {
    let mut c = enter_world(running).await;
    let gates = children(&mut c, 10, u32::from(gate), 1).await;
    assert_eq!(long(gates[0].0.get(0x06)), 0, "{at}: gate population");
    let info = PatData::new();
    send(&mut c, 11, ReqLayerDown { layer: gate, info }).await;
    expect::<AnsLayerDown>(&mut c).await;
    let cities = children(&mut c, 12, 1, 1).await;
    assert_eq!(long(cities[0].0.get(0x06)), 0, "{at}: city population");
    assert_eq!(
        cities[0].0.get(0x10),
        Some(&Item::Byte(1)),
        "{at}: city state"
    );
    meet(&mut c, &mut enter_world(running).await, gate, 2).await;
    let seen = drain(&mut c, Duration::from_millis(100)).await;
    assert!(
        seen.contains(&AnsLayerCreateFoot::ID),
        "{at}: city: {seen:?}"
    );
    let info = PatData::new();
    let extra = Default::default();
    send(&mut c, 30, ReqCircleCreate { info, extra }).await;
    expect::<NtcCircleListLayerChange>(&mut c).await;
    assert_eq!(
        expect::<AnsCircleCreate>(&mut c).await.circle,
        1,
        "{at}: quest"
    );
}

/// Tier 3, stress: consoles doing random things (seeded: a failure replays) in two gates, some vanishing and coming
/// back, some doing what a real game never does (a second reservation, a city created from anywhere, a quest departed
/// twice, a second login of the same hunter). Whatever they did, the server stays up, the world's invariants hold
/// after every action (`World::check`, through `Running::violations`) and, once they're all gone, nothing is left
/// behind.
#[tokio::test]
async fn random_play_leaves_no_trace() {
    const CONSOLES: u64 = 8;
    const ACTIONS: usize = 40;
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let seed = std::env::var("SEED").map_or(0x5eed, |s| s.parse().unwrap());
    eprintln!("SEED={seed}");
    let players = (0..CONSOLES).map(|i| {
        random_play(
            &running,
            seed ^ (i + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15),
            ACTIONS,
        )
    });
    let dropped: usize = futures_util::future::join_all(players)
        .await
        .into_iter()
        .sum();
    eprintln!("{dropped} connections dropped by the server");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        running.violations(),
        Vec::<String>::new(),
        "the world's invariants are broken (SEED={seed})"
    );
    for gate in [1, 2] {
        nothing_left(
            &running,
            gate,
            &format!("gate {gate} after random play (SEED={seed})"),
        )
        .await;
    }
    running.stop().await;
}

/// One console's random play: how many times the server dropped it.
async fn random_play(running: &server::Running, mut seed: u64, actions: usize) -> usize {
    let mut random = move |n: u64| {
        // xorshift64
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    let (code, _, _) = first_visit(Lmp::of(running), None).await;
    let mut c = play(running, &code).await;
    // The connections a second login of the same hunter left behind: kept open to the end.
    let mut older = Vec::new();
    let mut dropped = 0;
    for seq in 100..100 + actions as u16 {
        let n = random(3) as u16 + 1;
        let frames: Vec<Frame> = match random(19) {
            0 => vec![request(
                seq,
                ReqLayerDown {
                    layer: n.min(2),
                    info: PatData::new(),
                },
            )],
            1 => vec![request(seq, ReqLayerUp {})],
            2 => vec![
                request(seq, ReqLayerCreateHead { number: n }),
                request(
                    seq,
                    ReqLayerCreateSet {
                        number: n,
                        layer: PatData::new(),
                        extra: Default::default(),
                    },
                ),
                request(seq, ReqLayerCreateFoot { number: n, unk: 0 }),
            ],
            3 => vec![request(
                seq,
                ReqLayerDown {
                    layer: n,
                    info: PatData::new(),
                },
            )],
            4 => vec![request(
                seq,
                ReqCircleCreate {
                    info: PatData::new().with(0x09, Item::Long(4)),
                    extra: Default::default(),
                },
            )],
            5 => vec![request(
                seq,
                ReqCircleJoin {
                    circle: n.into(),
                    info: PatData::new(),
                },
            )],
            6 => vec![request(seq, ReqCircleLeave { circle: n.into() })],
            7 => vec![request(
                seq,
                ReqCircleMatchOptionSet {
                    options: PatData::new().with(0x03, Item::Byte(1)),
                },
            )],
            8 => vec![request(seq, ReqCircleMatchStart {})],
            9 => vec![request(
                seq,
                NtcLayerUserPosition {
                    data: wire::packets::Rest(position_lp2(&[0; 37]).into()),
                },
            )],
            10 => vec![request(seq, ReqLayerEnd {})],
            // What a real game never sends: a second reservation while holding one, a city created from wherever the
            // hunter is, a quest departed twice or by a leader not ready, party data into a quest it isn't in.
            11 => vec![
                request(seq, ReqLayerCreateHead { number: n }),
                request(seq, ReqLayerCreateHead { number: n % 3 + 1 }),
            ],
            12 => vec![request(
                seq,
                ReqLayerCreateSet {
                    number: n,
                    layer: PatData::new(),
                    extra: Default::default(),
                },
            )],
            13 => vec![
                request(seq, ReqCircleMatchStart {}),
                request(seq, ReqCircleMatchStart {}),
            ],
            14 => vec![
                request(
                    seq,
                    ReqCircleMatchOptionSet {
                        options: PatData::new().with(0x03, Item::Byte(0)),
                    },
                ),
                request(seq, ReqCircleMatchStart {}),
            ],
            15 => vec![request(
                seq,
                NtcCircleBinary {
                    circle: n.into(),
                    data: wire::packets::Rest(vec![0; 8].into()),
                },
            )],
            16 => vec![request(
                seq,
                NtcCircleBinary2 {
                    circle: n.into(),
                    capcom_id: Bytes::from_static(b"AAAAAA"),
                    data: wire::packets::Rest(vec![0; 8].into()),
                },
            )],
            17 => {
                // The same hunter logs in again, while the first connection is still open.
                older.push(std::mem::replace(&mut c, play(running, &code).await));
                continue;
            }
            _ => {
                // Vanishes, comes back as another console.
                let (code, _, _) = first_visit(Lmp::of(running), None).await;
                c = play(running, &code).await;
                continue;
            }
        };
        let frame_ids: Vec<u32> = frames.iter().map(|f| f.id.0).collect();
        let mut closed = false;
        for frame in frames {
            closed |= c.send(frame).await.is_err();
        }
        let window = Duration::from_millis(random(15) + 1);
        loop {
            match c.recv(window).await {
                Ok(Received::Frame(_)) => continue,
                Ok(Received::Timeout) => break,
                Ok(Received::Closed) | Err(_) => {
                    closed = true;
                    break;
                }
            }
        }
        if closed {
            dropped += 1;
            c = enter_world(running).await;
        }
        let violations = running.violations();
        assert!(
            violations.is_empty(),
            "the world's invariants are broken after action {} (frames of id {frame_ids:x?}): {violations:#?}",
            seq - 99
        );
    }
    drop(older);
    dropped
}

fn request<P: Packet>(seq: u16, packet: P) -> Frame {
    let mut payload = Vec::new();
    packet.encode(&mut payload).unwrap();
    Frame {
        seq,
        id: P::ID,
        payload: payload.into(),
    }
}

/// The NatNeg records a socket receives within 300 ms.
async fn natneg_received(socket: &tokio::net::UdpSocket) -> Vec<wire::natneg::Record> {
    let mut records = Vec::new();
    let mut buf = [0; 1500];
    while let Ok(Ok(n)) =
        tokio::time::timeout(Duration::from_millis(300), socket.recv(&mut buf)).await
    {
        records.push(wire::natneg::Record::decode(&buf[..n]).unwrap());
    }
    records
}

/// Two fake consoles (a game socket and a NatNeg socket each) negotiate a pair through the server: their sockets,
/// and the address each was told its peer is at (`CONNECT`). With `symmetric`, the first console's natneg2 `INIT`
/// comes from another port (a NAT mapping each destination apart).
async fn negotiate(
    running: &server::Running,
    cookie: u32,
    symmetric: bool,
) -> (
    Vec<(tokio::net::UdpSocket, tokio::net::UdpSocket)>,
    Vec<SocketAddr>,
) {
    use std::net::{Ipv4Addr, SocketAddrV4};
    use wire::natneg::{Body, Port, Record};

    let natneg = SocketAddr::from(([127, 0, 0, 1], running.natneg.unwrap().port()));
    let init = |kind, client_index| Record {
        cookie,
        body: Body::Init {
            port: Port { kind, client_index },
            use_game_port: 1,
            local: SocketAddrV4::new(Ipv4Addr::new(192, 168, 0, 2), 0),
            game: Bytes::from_static(b"mh3uswii"),
        },
    };
    let mut consoles = Vec::new();
    for _ in 0..2 {
        consoles.push((
            tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap(),
            tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap(),
        ));
    }
    let other_mapping = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    for (index, (game, nn)) in consoles.iter().enumerate() {
        let index = index as u8;
        let to_natneg2 = if symmetric && index == 0 {
            &other_mapping
        } else {
            nn
        };
        for (socket, kind) in [(game, 0), (nn, 1), (to_natneg2, 2), (nn, 3)] {
            socket
                .send_to(&init(kind, index).encode(), natneg)
                .await
                .unwrap();
        }
    }
    let mut told = Vec::new();
    for (index, (game, nn)) in consoles.iter().enumerate() {
        let mut got = natneg_received(game).await;
        got.extend(natneg_received(nn).await);
        let acks = got
            .iter()
            .filter(|r| matches!(r.body, Body::InitAck(_)))
            .count();
        let expected = if symmetric && index == 0 { 3 } else { 4 };
        assert_eq!(acks, expected, "console {index}: {got:?}");
        let connects: Vec<SocketAddr> = got
            .iter()
            .filter_map(|r| match &r.body {
                Body::Connect(p) => Some(SocketAddr::V4(p.addr)),
                _ => None,
            })
            .collect();
        assert!(
            !connects.is_empty() && connects.iter().all(|c| *c == connects[0]),
            "console {index}: {got:?}"
        );
        told.push(connects[0]);
    }
    (consoles, told)
}

/// Tier 5: two consoles negotiate through the server: each gets a `CONNECT` with the other's game socket address;
/// a report is acknowledged; nothing is answered for an address with no logged-in console. The QR check is answered.
#[tokio::test]
async fn natneg_pairs_two_consoles() {
    use wire::natneg::{Body, Port, Record};

    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let natneg = SocketAddr::from(([127, 0, 0, 1], running.natneg.unwrap().port()));
    let qr = SocketAddr::from(([127, 0, 0, 1], running.qr.unwrap().port()));

    // Nobody logged in: ignored.
    let stranger = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let report = Record {
        cookie: 1,
        body: Body::Report {
            port: Port {
                kind: 0x80,
                client_index: 0,
            },
            result: 1,
            nat_type: 6,
            mapping: 0,
            game: Bytes::from_static(b"mh3uswii"),
        },
    };
    stranger.send_to(&report.encode(), natneg).await.unwrap();
    assert!(natneg_received(&stranger).await.is_empty());

    let _logged_in = enter_world(&running).await;
    let (consoles, told) = negotiate(&running, 0x5a8f_90df, false).await;
    for index in 0..2 {
        assert_eq!(
            told[index],
            consoles[1 - index].0.local_addr().unwrap(),
            "console {index}"
        );
    }
    let (_, nn) = &consoles[0];
    nn.send_to(&report.encode(), natneg).await.unwrap();
    assert!(matches!(
        natneg_received(nn).await[..],
        [Record {
            body: Body::ReportAck(..),
            ..
        }]
    ));

    let check = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    check
        .send_to(&hex::decode("09000000006d6833757377696900").unwrap(), qr)
        .await
        .unwrap();
    let mut buf = [0; 64];
    let n = tokio::time::timeout(Duration::from_secs(1), check.recv(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(buf[..n], wire::natneg::AVAILABLE);
    running.stop().await;
}

/// Tier 5, `relay`: both consoles are told the same relay port; it forwards what one's game socket sends to the
/// other, both ways, and nothing for anyone else.
#[tokio::test]
async fn the_relay_forwards_a_pair() {
    let (mut config, _db) = config();
    config.strategy = server::config::Strategy::Relay;
    let running = server::start(&config).await.unwrap();
    let _logged_in = enter_world(&running).await;
    let (consoles, told) = negotiate(&running, 0x1234_5678, false).await;
    assert_eq!(told[0], told[1]);
    assert_eq!(
        told[0].ip().to_string(),
        "192.168.110.150",
        "EXTERNAL_ADDRESS"
    );
    let relay = SocketAddr::from(([127, 0, 0, 1], told[0].port()));
    let (a, b) = (&consoles[0].0, &consoles[1].0);
    let mut buf = [0; 64];
    // A speaks first (B's address isn't known yet: dropped), then B, then A again.
    a.send_to(b"ping a", relay).await.unwrap();
    b.send_to(b"ping b", relay).await.unwrap();
    let n = tokio::time::timeout(Duration::from_secs(1), a.recv(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&buf[..n], b"ping b");
    a.send_to(b"game a", relay).await.unwrap();
    let n = tokio::time::timeout(Duration::from_secs(1), b.recv(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&buf[..n], b"game a");
    // A stranger on another IP: never forwarded.
    let stranger = tokio::net::UdpSocket::bind("127.0.0.2:0").await;
    if let Ok(stranger) = stranger {
        stranger.send_to(b"spam", relay).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(300), b.recv(&mut buf))
                .await
                .is_err()
        );
    }
    running.stop().await;
}

/// Tier 2, UDP: every scenario's NatNeg and QR datagrams, from all its consoles merged in time order, sent to the
/// server (`p2p`, as AltWFC answered). Each socket must get what AltWFC sent it (each answer once or more), except
/// `CONNECT`s, compared per console (which of its sockets gets one differs, the game takes it on either): the same
/// peer game sockets (recorded ports mapped to the replay's). AltWFC's `CONNECT`s to a peer's NatNeg socket (a port
/// taken from the wrong `INIT`; the game ignores them) aren't expected.
#[tokio::test]
async fn natneg_exchanges_replay() {
    use std::collections::{BTreeSet, HashMap};
    use std::net::SocketAddrV4;
    use testkit::corpus::UdpDir;
    use wire::natneg::{Body, Record};

    let mut checked = 0;
    for scenario in scenarios() {
        let (config, _db) = config();
        let running = server::start(&config).await.unwrap();
        let _logged_in = enter_world(&running).await;
        let to_server = |remote: &str| match remote.rsplit(':').next() {
            Some("27900") => Some(running.qr.unwrap().port()),
            Some("27901") => Some(running.natneg.unwrap().port()),
            _ => None,
        };
        // (console, datagram), the server's only, in time order.
        let mut datagrams: Vec<_> = corpus::load(&scenario)
            .unwrap()
            .into_iter()
            .flat_map(|r| {
                let console = r.console.clone();
                r.datagrams.into_iter().map(move |d| (console.clone(), d))
            })
            .filter(|(_, d)| to_server(&d.remote).is_some())
            .collect();
        datagrams.sort_by_key(|(_, d)| d.t);
        if datagrams.is_empty() {
            running.stop().await;
            continue;
        }
        let decode = |payload: &str| Record::decode(&hex::decode(payload).unwrap());
        let mut sockets = HashMap::new();
        for local in datagrams
            .iter()
            .map(|(_, d)| d.local)
            .collect::<BTreeSet<_>>()
        {
            let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
            sockets.insert(local, socket);
        }
        let port_of: HashMap<u16, u16> = sockets
            .iter()
            .map(|(&recorded, s)| (recorded, s.local_addr().unwrap().port()))
            .collect();
        // Game sockets: the sources of port type 0 `INIT`s.
        let games: BTreeSet<u16> = datagrams
            .iter()
            .filter(|(_, d)| d.dir == UdpDir::Out)
            .filter(|(_, d)| {
                matches!(decode(&d.payload), Ok(Record { body: Body::Init { port, .. }, .. }) if port.kind == 0)
            })
            .map(|(_, d)| d.local)
            .collect();
        // Expected: per socket (not `CONNECT`), per console (`CONNECT`, recorded peer ports mapped).
        let mut want: HashMap<String, BTreeSet<String>> = HashMap::new();
        for (console, d) in datagrams.iter().filter(|(_, d)| d.dir == UdpDir::In) {
            match decode(&d.payload) {
                Ok(Record {
                    cookie,
                    body: Body::Connect(mut peer),
                }) => {
                    let Some(&port) = port_of
                        .get(&peer.addr.port())
                        .filter(|_| games.contains(&peer.addr.port()))
                    else {
                        continue;
                    };
                    peer.addr = SocketAddrV4::new(*peer.addr.ip(), port);
                    let connect = Record {
                        cookie,
                        body: Body::Connect(peer),
                    }
                    .encode();
                    want.entry(console.clone())
                        .or_default()
                        .insert(hex::encode(connect));
                }
                _ => {
                    want.entry(d.local.to_string())
                        .or_default()
                        .insert(d.payload.clone());
                }
            }
        }
        let console_of: HashMap<u16, String> = datagrams
            .iter()
            .map(|(console, d)| (d.local, console.clone()))
            .collect();
        for (_, d) in datagrams.iter().filter(|(_, d)| d.dir == UdpDir::Out) {
            let to = SocketAddr::from(([127, 0, 0, 1], to_server(&d.remote).unwrap()));
            sockets[&d.local]
                .send_to(&hex::decode(&d.payload).unwrap(), to)
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let mut got: HashMap<String, BTreeSet<String>> = HashMap::new();
        for (recorded, socket) in &sockets {
            let mut buf = [0; 1500];
            while let Ok(Ok(n)) =
                tokio::time::timeout(Duration::from_millis(200), socket.recv(&mut buf)).await
            {
                let key = match Record::decode(&buf[..n]) {
                    Ok(Record {
                        body: Body::Connect(_),
                        ..
                    }) => console_of[recorded].clone(),
                    _ => recorded.to_string(),
                };
                got.entry(key).or_default().insert(hex::encode(&buf[..n]));
            }
        }
        let keys: BTreeSet<&String> = want.keys().chain(got.keys()).collect();
        for key in keys {
            let (g, w) = (
                got.get(key).cloned().unwrap_or_default(),
                want.get(key).cloned().unwrap_or_default(),
            );
            assert_eq!(g, w, "{}: {key}", scenario.display());
            checked += w.len();
        }
        running.stop().await;
    }
    if !scenarios().is_empty() {
        assert!(checked > 0, "no NatNeg answers checked");
    }
}

/// Tier 5, `hybrid`: a pair connects directly; after its failure report, the next pair of those consoles (the
/// player retrying) goes through the relay. A symmetric NAT is relayed at once.
#[tokio::test]
async fn hybrid_relays_after_a_failure() {
    use wire::natneg::{Body, Port, Record};

    let hybrid = || {
        let (mut config, db) = config();
        config.strategy = server::config::Strategy::Hybrid;
        (config, db)
    };
    let relayed = |told: &[SocketAddr]| told[0] == told[1];

    let (config, _db) = hybrid();
    let running = server::start(&config).await.unwrap();
    let natneg = SocketAddr::from(([127, 0, 0, 1], running.natneg.unwrap().port()));
    let _logged_in = enter_world(&running).await;
    let (consoles, told) = negotiate(&running, 1, false).await;
    assert!(!relayed(&told), "first try: direct");
    let failure = Record {
        cookie: 1,
        body: Body::Report {
            port: Port {
                kind: 0x80,
                client_index: 0,
            },
            result: 0,
            nat_type: 6,
            mapping: 0,
            game: Bytes::from_static(b"mh3uswii"),
        },
    };
    consoles[0]
        .1
        .send_to(&failure.encode(), natneg)
        .await
        .unwrap();
    natneg_received(&consoles[0].1).await;
    let (_, told) = negotiate(&running, 2, false).await;
    assert!(relayed(&told), "the retry: relayed");
    running.stop().await;

    let (config, _db) = hybrid();
    let running = server::start(&config).await.unwrap();
    let _logged_in = enter_world(&running).await;
    let (_, told) = negotiate(&running, 3, true).await;
    assert!(relayed(&told), "a symmetric NAT: relayed at once");
    running.stop().await;
}

/// Tier 3: a quest leader whose console vanishes (a crash, a lost connection) leaves cleanly: before departure the
/// party is cancelled for the others, after departure the next member leads it; the city gets a new host.
#[tokio::test]
async fn a_vanishing_leader_is_cleaned_up() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();

    let (a, mut b) = party(&running, 1, false).await;
    drop(a);
    let seen = drain(&mut b, Duration::from_millis(500)).await;
    for expected in [
        NtcCircleLeave::ID,
        NtcCircleBreak::ID,
        NtcCircleListLayerChange::ID,
        NtcLayerOut::ID,
        NtcLayerHost::ID,
    ] {
        assert!(
            seen.contains(&expected),
            "before departure: no {expected} in {seen:?}"
        );
    }
    // B is out of the party: it can create its own quest.
    send(
        &mut b,
        30,
        ReqCircleCreate {
            info: PatData::new(),
            extra: Default::default(),
        },
    )
    .await;
    let seen = drain(&mut b, Duration::from_millis(300)).await;
    assert!(seen.contains(&AnsCircleCreate::ID), "{seen:?}");

    let (a, mut b) = party(&running, 2, true).await;
    drop(a);
    let seen = drain(&mut b, Duration::from_millis(500)).await;
    for expected in [NtcCircleLeave::ID, NtcCircleHost::ID, NtcLayerOut::ID] {
        assert!(
            seen.contains(&expected),
            "after departure: no {expected} in {seen:?}"
        );
    }
    assert!(
        !seen.contains(&NtcCircleBreak::ID),
        "a departed quest goes on: {seen:?}"
    );
    running.stop().await;
}

/// Our Nintendo login (`nas.rs`, ADR 0013): its three actions over HTTP, in the game's base64; its
/// token checked at OPN, used up at LMP's first visit, which links the console to its Wii.
#[tokio::test]
async fn our_nintendo_login() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let nas = running.nas;
    let field = |reply: &[(String, Vec<u8>)], key: &str| {
        let value = reply.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        String::from_utf8(value.unwrap_or_default()).unwrap()
    };
    let created = testkit::nas::ac(nas, &[("action", b"acctcreate")])
        .await
        .unwrap();
    assert_eq!(field(&created, "returncd"), "002");
    let user_id = field(&created, "userid");
    assert!(
        user_id.len() == 13 && user_id.bytes().all(|b| b.is_ascii_digit()),
        "{user_id}"
    );
    let fields: [(&str, &[u8]); 2] = [("action", b"login"), ("userid", user_id.as_bytes())];
    let login = testkit::nas::ac(nas, &fields).await.unwrap();
    assert_eq!(field(&login, "returncd"), "001");
    // A lab Dolphin's login, as captured in the first console run (2026-10-11): URL-encoded on top of the base64.
    let recorded = "action=bG9naW4%2A&gsbrcd=&userid=MDAwMDAwMDAwMDAwMg%2A%2A&ingamesn=&sdkver=MDAxMDAw&\
        gamecd=Uk1IUA%2A%2A&makercd=MDg%2A&unitcd=MQ%2A%2A&macadr=MDAxN2FiYjMzYjJi&lang=MDE%2A&\
        devtime=MjYxMDA5MTk1MjQ5&csnum=TEVINzkxNDY0MzE3&cfc=Mjc5MDQ5MTU3ODg5NTg5OQ%2A%2A&region=MDI%2A";
    let login = testkit::nas::post(nas, recorded).await.unwrap();
    assert_eq!(field(&login, "returncd"), "001");
    let token = testkit::nas::svcloc(nas, "0017abcdef01", "LEH123456789")
        .await
        .unwrap();
    assert!(
        token.len() == 83 && token.starts_with(b"NDS"),
        "Nintendo's form"
    );
    // Refused: an unknown action, a MAC that isn't one, a form that isn't base64 in the game's alphabet.
    assert!(
        testkit::nas::ac(nas, &[("action", b"other")])
            .await
            .is_err()
    );
    assert!(
        testkit::nas::svcloc(nas, "nomac", "LEH123456789")
            .await
            .is_err()
    );

    // OPN: a token from another login is refused (a second `NtcLogin`) with a text; ours is accepted, only looked at.
    for (token, ours) in [(vec![b'N'; 83], false), (token.clone(), true)] {
        let (mut opn, status) = opn_connect(running.opn, b"RMHP08", &NATNEG_HOSTNAMES).await;
        assert_eq!(status, 5);
        let token = token.into();
        send(&mut opn, 1, ReqAuthenticationToken { token }).await;
        match ours {
            true => {
                expect::<AnsAuthenticationToken>(&mut opn).await;
            }
            false => {
                // A second `NtcLogin`, refused, then the text.
                assert_eq!(expect::<NtcLogin>(&mut opn).await.status, 1);
                let text = refusal_text(&mut opn, 2).await;
                assert!(text.contains("didn't log in through this server"), "{text}");
            }
        }
    }

    // LMP: without a token, or with one used already, a first visit is refused with that message.
    let lmp = Lmp::of(&running);
    let with = |token: Option<Vec<u8>>| async move {
        let mut console = Console::connect(lmp.addr, false).await.unwrap();
        expect::<ReqConnection>(&mut console).await;
        let data = connection_data(None, None);
        send(&mut console, 0, AnsConnection { data }).await;
        assert_eq!(expect::<NtcLogin>(&mut console).await.status, 1);
        let data = match token {
            Some(token) => PatData::new().with(0x0a, Item::Binary(token.into())),
            None => PatData::new(),
        };
        send(&mut console, 1, ReqLoginInfo { data }).await;
        console
    };
    let text = expect_alert(&mut with(None).await, AnsLoginInfo::ID).await;
    assert!(text.contains("Log in again"), "{text}");
    expect::<AnsLoginInfo>(&mut with(Some(token.clone())).await).await;
    expect_alert(&mut with(Some(token)).await, AnsLoginInfo::ID).await;
    let db = server::database::open(&config.database_url).await.unwrap();
    let linked: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM devices), (SELECT count(*) FROM consoles WHERE device_id IS NOT NULL)",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(linked, (1, 1));
    running.stop().await;
}

/// A ban holds on the Wii (Nintendo login D1, D5): a new save on it is refused at OPN, then at LMP; another Wii isn't;
/// unbanned, it plays again.
#[tokio::test]
async fn a_ban_holds_on_its_wii() {
    let (mut config, _db) = config();
    let (running, mut admin, _) = with_admin(&mut config).await;
    let wii = |n| Lmp {
        wii: Some(n),
        ..Lmp::of(&running)
    };
    let (code, _, _) = first_visit(wii(900), None).await;
    login(wii(900), &code, 1).await;
    let capcom_id = first_visit(wii(900), Some(&code)).await.2[0].0.clone();
    assert_eq!(
        say(&mut admin, 20, &format!("/ban {capcom_id} cheating")).await,
        [format!("{capcom_id} banned.")]
    );

    // A new save on the banned Wii: refused at OPN, and at LMP should it skip OPN's check.
    let token = testkit::nas::svcloc(running.nas, "0017ab000384", "LEH000000900")
        .await
        .unwrap();
    let (mut opn, _) = opn_connect(running.opn, b"RMHP08", &NATNEG_HOSTNAMES).await;
    send(
        &mut opn,
        1,
        ReqAuthenticationToken {
            token: token.into(),
        },
    )
    .await;
    assert_eq!(expect::<NtcLogin>(&mut opn).await.status, 1);
    let text = refusal_text(&mut opn, 2).await;
    assert!(text.contains("banned"), "{text}");
    match login_info(wii(900), None).await {
        Some(Received::Frame(f)) => assert_eq!(f.id.0, AnsLoginInfo::ID.0 | 0x01, "an alert"),
        other => panic!("expected an alert, got {other:?}"),
    }
    // Another Wii plays.
    first_visit(wii(901), None).await;

    assert_eq!(
        say(&mut admin, 21, &format!("/unban {capcom_id}")).await,
        [format!("{capcom_id} unbanned.")]
    );
    first_visit(wii(900), None).await;
    running.stop().await;
}

/// In maintenance mode the Nintendo login still answers: the console must reach OPN to read why it's refused.
#[tokio::test]
async fn the_nintendo_login_answers_in_maintenance() {
    let (mut config, _db) = config();
    config.maintenance = Some("Back soon.".into());
    let running = server::start(&config).await.unwrap();
    testkit::nas::svcloc(running.nas, "0017abcdef01", "LEH123456789")
        .await
        .unwrap();
    running.stop().await;
}

/// A refused console, once told why, is only let go (the user, 2026-10-11): any other request closes its connection
/// at once, its goodbye is answered, and a connection left open is closed after the grace (10 s), with no line check
/// meanwhile.
#[tokio::test]
async fn a_refused_console_is_only_let_go() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let refused = || async {
        let other_patch = b"natneg1.gs.example.com\0natneg2.gs.example.com\0";
        let (mut console, status) = opn_connect(running.opn, b"RMHP08", other_patch).await;
        assert_eq!(status, 1);
        let token = Bytes::from_static(b"NDS-from-any-login");
        send(&mut console, 1, ReqAuthenticationToken { token }).await;
        expect::<AnsAuthenticationToken>(&mut console).await;
        assert!(refusal_text(&mut console, 2).await.contains("not patched"));
        console
    };
    // Closed: over TLS without its goodbye, which reads as an error.
    let closed = |r: anyhow::Result<Received>| matches!(r, Ok(Received::Closed) | Err(_));
    let mut pushy = refused().await;
    send(&mut pushy, 3, ReqTermsVersion {}).await;
    assert!(closed(pushy.recv(Duration::from_secs(2)).await));
    let mut lingering = refused().await;
    send(&mut lingering, 3, ReqShut { kind: 1 }).await;
    expect::<AnsShut>(&mut lingering).await;
    let start = std::time::Instant::now();
    assert!(closed(lingering.recv(Duration::from_secs(15)).await));
    assert!(
        start.elapsed() >= Duration::from_secs(9),
        "{:?}",
        start.elapsed()
    );
    running.stop().await;
}

/// What `tracing` events a test's thread writes, as text.
#[derive(Clone, Default)]
struct Log(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Log {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Log {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

/// Stopping the server waits for the tasks that outlive a request (mh3-rs/server#68): the connections, which close
/// their handler (a hunter leaves the world), the relays, a stop's countdown, the database's retry. A task of the
/// test's own on the server's tracker, which ends a moment after the server is told to stop, stands for them: it has
/// ended when `stop` returns, where `stop` used to return once the listeners had.
#[tokio::test]
async fn stop_waits_for_the_tasks_it_runs() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let (stopping, ended) = (
        running.stopping(),
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    running.tasks().spawn({
        let ended = ended.clone();
        async move {
            stopping.cancelled().await;
            tokio::time::sleep(Duration::from_millis(300)).await;
            ended.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    });
    running.stop().await;
    assert!(
        ended.load(std::sync::atomic::Ordering::SeqCst),
        "`stop` returned before a task of the server ended"
    );
}

/// A stop under way doesn't keep the server from stopping: its countdown ends with the server (`Running::stop` waits
/// for it now, with the connections).
#[tokio::test]
async fn a_pending_stop_does_not_keep_the_server_from_stopping() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    running.stop_in(Duration::from_secs(3600));
    tokio::time::timeout(Duration::from_secs(10), running.stop())
        .await
        .expect("stop() waits for the countdown of an hour");
}

/// A key that left the server (no breaking change since the image is public) is accepted: the server starts, warns
/// about it, and listens on nothing there. The quest bots' port (tag `archive/quest-bots`) is the example.
#[tokio::test]
async fn a_removed_key_is_ignored_with_a_warning() {
    let (mut config, _db) = config();
    // A port nobody listens on, so that "nothing listens there" is a fact the test can check.
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = free.local_addr().unwrap().port();
    drop(free);
    config.ignored = Config::parse(&format!("bot_port = {port}"))
        .unwrap()
        .ignored;
    let log = Log::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer({
            let log = log.clone();
            move || log.clone()
        })
        .with_ansi(false)
        .finish();
    // Thread-local: this test runs on one thread (the `tokio::test` runtime), so another test's logs stay out.
    let _logging = tracing::subscriber::set_default(subscriber);
    let running = server::start(&config).await.unwrap();
    let text = log.text();
    assert!(
        text.contains("WARN") && text.contains("bot_port") && text.contains("ignored"),
        "{text}"
    );
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err(),
        "something listens on the bot port"
    );
    // The server is up all the same.
    assert_eq!(http_status(running.public, "/health/live").await, "200");
    running.stop().await;
}

/// A database written by the published server (migration 0001 alone) with a console banned by a user of the old API:
/// the next release upgrades it in place at its start, and the console is refused at login all the same, while one that
/// was never banned logs in. The ban's author is gone with the user, the ban is not.
#[tokio::test]
async fn a_ban_from_before_the_upgrade_still_refuses() {
    use openssl::hash::MessageDigest;
    use openssl::pkey::PKey;
    use openssl::sign::Signer;
    use sqlx::migrate::Migrator;
    use sqlx::sqlite::SqliteConnectOptions;

    let (config, dir) = config();
    // The support codes' HMAC, as the server keys it: 11 characters of `0-9A-Z` (`accounts.rs`).
    let secret = config.secret.as_ref().unwrap().0.clone();
    let hmac = |code: &str| {
        let key = PKey::hmac(&secret).unwrap();
        let mut signer = Signer::new(MessageDigest::sha256(), &key).unwrap();
        signer.update(code.as_bytes()).unwrap();
        signer.sign_to_vec().unwrap()
    };
    let (banned, innocent) = ("BANNED00000", "INNOCENT000");

    // The published server's database: 0001 alone, by sqlx's own migrator over a folder that holds only that file.
    let only_0001 = dir.path().join("only_0001");
    std::fs::create_dir(&only_0001).unwrap();
    std::fs::copy(
        repo().join("migrations/0001_schema.sql"),
        only_0001.join("0001_schema.sql"),
    )
    .unwrap();
    let options = config
        .database_url
        .parse::<SqliteConnectOptions>()
        .unwrap()
        .create_if_missing(true)
        .foreign_keys(true);
    let old = sqlx::SqlitePool::connect_with(options).await.unwrap();
    Migrator::new(only_0001.as_path())
        .await
        .unwrap()
        .run(&old)
        .await
        .unwrap();
    for code in [banned, innocent] {
        sqlx::query(
            "INSERT INTO consoles (public_id, support_code_hmac, region) VALUES (randomblob(16), ?1, 'RMHP08')",
        )
        .bind(hmac(code))
        .execute(&old)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO users (public_id, email, role_id) VALUES (randomblob(16), 'admin@example.org', 2)",
    )
    .execute(&old)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO access_rules (public_id, kind_id, console_id, reason, created_by)
         SELECT randomblob(16), rule_kinds.id, consoles.id, 'cheating', 1 FROM rule_kinds, consoles
         WHERE rule_kinds.name = 'ban' AND consoles.support_code_hmac = ?1",
    )
    .bind(hmac(banned))
    .execute(&old)
    .await
    .unwrap();
    // It was there: a user, and the ban made by that user.
    let author: Option<i64> = sqlx::query_scalar("SELECT created_by FROM access_rules")
        .fetch_one(&old)
        .await
        .unwrap();
    assert_eq!(author, Some(1));
    old.close().await;

    let running = server::start(&config).await.unwrap();
    let lmp = Lmp::of(&running);
    match login_info(lmp, Some(banned)).await {
        Some(Received::Frame(f)) => assert_eq!(f.id.0, AnsLoginInfo::ID.0 | 0x01, "an alert"),
        other => panic!("expected an alert, got {other:?}"),
    }
    match login_info(lmp, Some(innocent)).await {
        Some(Received::Frame(f)) => assert_eq!(f.id, AnsLoginInfo::ID),
        other => panic!("expected a login, got {other:?}"),
    }
    // The ban is still one, now by no one.
    let db = server::database::open(&config.database_url).await.unwrap();
    let by: Option<i64> = sqlx::query_scalar("SELECT created_by FROM access_rules")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(by, None);
    running.stop().await;
}

/// A `POST` with a short body: the status code (the body is never what is being tested).
async fn http_post(addr: SocketAddr, path: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("POST {path} HTTP/1.0\r\nContent-Length: 5\r\n\r\nhello").as_bytes())
        .await
        .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    out.split_whitespace().nth(1).unwrap_or_default().to_owned()
}

/// Nothing that changes the server is on a port: `POST /announce` and `POST /stop` are on neither HTTP port (the
/// operator's commands go through the socket), and the game's own answers nothing but its login.
#[tokio::test]
async fn no_command_is_on_a_port() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let mut console = enter_world(&running).await;
    for addr in [running.public, running.nas] {
        for path in ["/announce", "/stop"] {
            assert_eq!(http_post(addr, path).await, "404", "{addr} {path}");
        }
    }
    // Nothing was said to the player, and nothing is stopping.
    assert!(
        drain(&mut console, Duration::from_millis(300))
            .await
            .is_empty()
    );
    assert!(!running.stopping().is_cancelled());
    running.stop().await;
}

/// The commands' socket exists by its name alone, while the server runs: an abstract socket, no file (Linux lists
/// them in `/proc/net/unix` with a leading `@`). It goes with the server.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_commands_socket_is_a_name_and_no_file() {
    let (config, _db) = config();
    let listed = |name: &str| {
        std::fs::read_to_string("/proc/net/unix")
            .unwrap()
            .lines()
            .any(|l| l.ends_with(&format!(" @{name}")))
    };
    assert!(!listed(&config.control), "there before the server starts");
    let running = server::start(&config).await.unwrap();
    assert!(listed(&config.control), "the server's socket");
    running.stop().await;
    assert!(!listed(&config.control), "it goes with the server");
}

/// A command with no server to receive it says so, naming the socket: it never hangs or answers as if it had worked.
#[cfg(target_os = "linux")]
#[test]
fn a_command_with_no_server_fails_clearly() {
    let name = format!("mh3sp-test-nobody-{}", std::process::id());
    for result in [
        server::control::announce(&name, "hello"),
        server::control::stop(&name, "5min"),
    ] {
        let error = format!("{:#}", result.unwrap_err());
        assert!(error.contains("no running server"), "{error}");
        assert!(error.contains(&name), "{error}");
    }
}

/// A second server in the same network namespace finds the socket's name taken: it starts all the same, says so, and
/// the first keeps its commands.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_taken_socket_name_does_not_stop_the_server() {
    let (first_config, _first_db) = config();
    let first = server::start(&first_config).await.unwrap();
    let (mut config, _db) = config();
    config.control = first_config.control.clone();
    let log = Log::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer({
            let log = log.clone();
            move || log.clone()
        })
        .with_ansi(false)
        .finish();
    let _logging = tracing::subscriber::set_default(subscriber);
    let second = server::start(&config).await.unwrap();
    let text = log.text();
    assert!(
        text.contains("WARN") && text.contains("commands' socket is not available"),
        "{text}"
    );
    assert_eq!(http_status(second.public, "/health/live").await, "200");
    // The name still reaches the first one: its console hears the announcement, the second's none.
    let mut heard = enter_world(&first).await;
    let name = config.control.clone();
    tokio::task::spawn_blocking(move || server::control::announce(&name, "Still here"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        &expect::<NtcLayerChat>(&mut heard).await.text[..],
        b"Still here"
    );
    second.stop().await;
    first.stop().await;
}

/// `mh3sp check` with nothing on port 80 fails the login's own line, with what to forward, and only that one.
#[tokio::test]
async fn check_fails_a_closed_login_port() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let report = check_report(&running, |p| p.nas = port).await;
    let failed = check_failures(&report);
    assert_eq!(failed.len(), 1, "{report}");
    assert!(failed[0].contains("(Nintendo login)"), "{report}");
    assert!(
        failed[0].contains(&format!("forward TCP {port}")),
        "{report}"
    );
    assert_eq!(check_passes(&report), 4, "{report}");
    running.stop().await;
}
