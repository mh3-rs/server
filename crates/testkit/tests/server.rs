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
const NATNEG_HOSTNAMES: &[u8] = b"natneg1.gs.nintendowifi.net\0natneg2.gs.nintendowifi.net\0";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

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
        health: any,
        api: any,
        qr: any,
        natneg: any,
        strategy: server::config::Strategy::P2p,
        relay_ports: 41000..=41099,
        tls_cert: repo().join("certs/dev/server.crt"),
        tls_key: repo().join("certs/dev/server.key"),
        database_url: format!("sqlite://{}", dir.path().join("mh3sp.db").display()),
        // Where most recordings were made; a replay uses its scenario's own (`recorded_address`).
        external_address: Some("192.168.110.150".into()),
        natneg_hostname: Some("natneg1.gs.nintendowifi.net".into()),
        secret: Some(server::config::Secret(b"test secret".to_vec())),
        // MH3SP's event files (`scripts/ci-tests.sh` provides them).
        content_dir: std::env::var_os("CONTENT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| repo().join("../reference")),
        encryption: false,
        admins: Vec::new(),
        bot_port: None,
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

/// A `GET` on the health port: status code and body.
async fn http_get(addr: SocketAddr, path: &str) -> (String, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.0\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let status = out.split_whitespace().nth(1).unwrap_or_default().to_owned();
    let body = out.split_once("\r\n\r\n").map_or("", |(_, b)| b).to_owned();
    (status, body)
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
    if status != 2 {
        return (status, None);
    }
    send(&mut console, 1, ReqMaintenance {}).await;
    let text = expect::<AnsMaintenance>(&mut console).await.text;
    (status, Some(String::from_utf8(text.to_vec()).unwrap()))
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
    let (lmp, fmp) = (running.lmp.unwrap(), running.fmp.unwrap());
    // OPN, for two consoles at the same address: each its key, which goes on at `ReqShut`.
    let mut keys = Vec::new();
    for _ in 0..2 {
        let (mut opn, _) = opn_connect(running.opn, b"RMHP08", NATNEG_HOSTNAMES).await;
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
        let mut console = open_encrypted(lmp, key, None, 1).await;
        let data = PatData::new();
        send(&mut console, 1, ReqLoginInfo { data }).await;
        expect::<AnsLoginInfo>(&mut console).await;
        let first = ticket_and_shut(&mut console, 2).await;
        // LMP's second visit: a slot, a Capcom ID, the ticket for FMP.
        let mut console = open_encrypted(lmp, key, Some(first), 2).await;
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
    let mut stranger = Console::connect(lmp, false).await.unwrap();
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
        let (mut console, status) = opn_connect(running.opn, b"RMHP08", NATNEG_HOSTNAMES).await;
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

    assert_eq!(http_status(running.health, "/health/live").await, "200");
    assert_eq!(http_status(running.health, "/health/ready").await, "200");
    assert_eq!(http_status(running.health, "/elsewhere").await, "404");
    // `mh3sp healthcheck` is blocking (it runs as its own process in the container).
    let health = running.health;
    tokio::task::spawn_blocking(move || server::health::check(health))
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
        opn_login(opn, b"RMHE08", NATNEG_HOSTNAMES),
        opn_login(
            opn,
            b"RMHP08",
            b"natneg1.gs.example.com\0natneg2.gs.example.com\0"
        ),
        opn_login(opn, b"RMHJ08", b""),
    );
    running.stop().await;
    assert_eq!(ok, (5, None));
    assert_eq!(unpatched.0, 2);
    assert!(unpatched.1.unwrap().contains("not patched"));
    assert_eq!(unsupported.0, 2);
    assert!(unsupported.1.unwrap().contains("not supported"));
}

#[tokio::test]
async fn refuses_everyone_when_not_configured() {
    let (mut config, _db) = config();
    config.natneg_hostname = None;
    let running = server::start(&config).await.unwrap();
    let (status, text) = opn_login(running.opn, b"RMHP08", NATNEG_HOSTNAMES).await;
    assert_eq!(status, 2);
    assert!(text.unwrap().contains("not configured"));
    assert_eq!(http_status(running.health, "/health/live").await, "200");
    assert_eq!(http_status(running.health, "/health/ready").await, "503");
    running.stop().await;
}

#[tokio::test]
async fn maintenance_mode_refuses_with_the_message() {
    let (mut config, _db) = config();
    config.maintenance = Some("Back at 18:00 UTC".into());
    let running = server::start(&config).await.unwrap();
    assert_eq!((running.lmp, running.fmp), (None, None));
    let login = opn_login(running.opn, b"RMHP08", NATNEG_HOSTNAMES).await;
    assert_eq!(login, (2, Some("Back at 18:00 UTC".into())));
    assert_eq!(http_status(running.health, "/health/live").await, "200");
    assert_eq!(http_status(running.health, "/health/ready").await, "503");
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
    assert_eq!(http_status(running.health, "/health/live").await, "503");
    let (status, text) = opn_login(running.opn, b"RMHP08", NATNEG_HOSTNAMES).await;
    assert_eq!(status, 2);
    assert!(text.unwrap().contains("not ready"));

    std::fs::create_dir(&folder).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while http_status(running.health, "/health/live").await != "200" {
        assert!(
            tokio::time::Instant::now() < deadline,
            "database never opened"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(http_status(running.health, "/health/ready").await, "200");
    assert_eq!(
        opn_login(running.opn, b"RMHP08", NATNEG_HOSTNAMES).await,
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

/// LMP's first visit: the support code (issued when `None`), the ticket, and the 6 slots' Capcom IDs.
async fn first_visit(
    lmp: SocketAddr,
    support_code: Option<&str>,
) -> (String, String, Vec<(String, String)>) {
    let mut console = Console::connect(lmp, false).await.unwrap();
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
    send(
        &mut console,
        1,
        ReqLoginInfo {
            data: PatData::new(),
        },
    )
    .await;
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
async fn login(lmp: SocketAddr, support_code: &str, slot: u32) -> String {
    let (_, ticket, _) = first_visit(lmp, Some(support_code)).await;
    let mut console = Console::connect(lmp, false).await.unwrap();
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
    let (lmp, fmp) = (running.lmp.unwrap(), running.fmp.unwrap());
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
    let lmp = running.lmp.unwrap();

    // A first-time console gets a support code and empty slots; its first hunter gets a Capcom ID.
    let (code, ticket, slots) = first_visit(lmp, None).await;
    assert_eq!(code.len(), 11);
    assert!(slots.iter().all(|(id, _)| id == "******"));
    let capcom_id = second_visit(lmp, &ticket, 2, Some("Aiko")).await;
    assert_eq!(capcom_id.len(), 6);

    // A redeemed ticket doesn't work twice: the connection is dropped.
    let mut reused = Console::connect(lmp, false).await.unwrap();
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
    assert_ne!(second_visit(lmp, &ticket, 2, Some("Aiko")).await, capcom_id);
    running.stop().await;

    // After a restart, the console finds its hunter in the same slot, with the same Capcom ID.
    let running = server::start(&config).await.unwrap();
    let lmp = running.lmp.unwrap();
    let (again, ticket, slots) = first_visit(lmp, Some(&code)).await;
    assert_eq!(again, code);
    assert_eq!(slots[1], (capcom_id.clone(), "Aiko".to_owned()));
    // Picking it without a name (it was listed with its hunter's) keeps the name.
    assert_eq!(second_visit(lmp, &ticket, 2, None).await, capcom_id);
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
    let lmp = running.lmp.unwrap();
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

/// An API request: status code and body. `auth` is a whole header line (`Authorization: …` or `Cookie: …`).
async fn api(addr: SocketAddr, method: &str, path: &str, auth: &str, body: &str) -> (u16, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: test\r\n{auth}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(request.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let status = out.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = out.split_once("\r\n\r\n").map_or("", |(_, b)| b).to_owned();
    (status, body)
}

/// The API (`api.rs`): tokens from `users.rs`, as a Bearer header or the cookie; admin routes refuse players;
/// a kick through it drops the player.
#[tokio::test]
async fn the_api_serves_admins_only() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let addr = running.api.unwrap();
    let db = server::database::open(&config.database_url).await.unwrap();
    use server::users::{Role, add, issue, revoke};
    add(&db, "admin@example.org", Role::Admin).await.unwrap();
    add(&db, "player@example.org", Role::Player).await.unwrap();
    let admin = format!(
        "Authorization: Bearer {}",
        issue(&db, "admin@example.org", "cli").await.unwrap()
    );
    let player = issue(&db, "player@example.org", "session").await.unwrap();
    let player = format!("Cookie: theme=dark; mh3sp_token={player}");

    let (status, body) = api(addr, "GET", "/api/me", &admin, "").await;
    assert_eq!(status, 200);
    let me: shared::Me = serde_json::from_str(&body).unwrap();
    assert_eq!(me.role, "admin");
    let (status, body) = api(addr, "GET", "/api/me", &player, "").await;
    assert_eq!(
        (
            status,
            serde_json::from_str::<shared::Me>(&body).unwrap().role
        ),
        (200, "player".into())
    );
    assert_eq!(api(addr, "GET", "/api/me", "X-None: 1", "").await.0, 401);
    assert_eq!(
        api(
            addr,
            "GET",
            "/api/me",
            "Authorization: Bearer mh3sp_0.0",
            ""
        )
        .await
        .0,
        401
    );
    assert_eq!(api(addr, "GET", "/api/players", &player, "").await.0, 403);
    assert_eq!(api(addr, "GET", "/api/bans", &player, "").await.0, 403);
    let announce = r#"{"text":"hello"}"#;
    // Every admin route refuses a player (a body valid for each: unknown fields are ignored).
    let any = r#"{"text":"x","delay":"1h","player":"X","reason":"r","bot":"X"}"#;
    for route in [
        "announce",
        "stop",
        "kick",
        "tell",
        "ban",
        "unban",
        "bot/add",
        "bot/remove",
    ] {
        let path = format!("/api/{route}");
        assert_eq!(
            api(addr, "POST", &path, &player, any).await.0,
            403,
            "{route}"
        );
    }
    assert_eq!(
        api(addr, "POST", "/api/announce", &player, announce)
            .await
            .0,
        403
    );

    let mut console = enter_world(&running).await;
    let (status, body) = api(addr, "GET", "/api/players", &admin, "").await;
    assert_eq!(status, 200);
    let online: Vec<shared::Player> = serde_json::from_str(&body).unwrap();
    assert_eq!(online.len(), 1);
    assert_eq!(
        (online[0].name.as_str(), online[0].server.as_str()),
        ("Aiko", "Valor1")
    );

    let (status, body) = api(addr, "POST", "/api/announce", &admin, announce).await;
    assert_eq!(
        (status, body.contains("Sent to 1 players.")),
        (200, true),
        "{body}"
    );
    expect::<NtcLayerChat>(&mut console).await;
    // A long paste would fill every mailbox (a frame per 30 characters): refused.
    let long = format!(r#"{{"text":"{}"}}"#, "word ".repeat(61));
    assert_eq!(
        api(addr, "POST", "/api/announce", &admin, &long).await.0,
        400
    );
    // The quest bots (this server has them, no brain connected).
    let bot = format!(r#"{{"player":"{}"}}"#, online[0].capcom_id);
    let (status, body) = api(addr, "POST", "/api/bot/add", &admin, &bot).await;
    assert_eq!((status, body.contains("NoBrain")), (409, true), "{body}");
    let gone = r#"{"bot":"NOBOT1"}"#;
    assert_eq!(
        api(addr, "POST", "/api/bot/remove", &admin, gone).await.0,
        404
    );
    let tell = format!(r#"{{"player":"{}","text":"psst"}}"#, online[0].capcom_id);
    assert_eq!(api(addr, "POST", "/api/tell", &admin, &tell).await.0, 200);
    assert_eq!(
        &expect::<NtcLayerChat>(&mut console).await.text[..],
        b"psst"
    );
    assert_eq!(
        api(addr, "POST", "/api/kick", &admin, r#"{"player":"NOBODY"}"#)
            .await
            .0,
        404
    );
    assert_eq!(
        api(addr, "POST", "/api/stop", &admin, r#"{"delay":"soon"}"#)
            .await
            .0,
        400
    );
    assert_eq!(
        api(addr, "POST", "/api/stop", &admin, r#"{"delay":"cancel"}"#)
            .await
            .0,
        409
    );
    let kick = format!(r#"{{"player":"{}"}}"#, online[0].capcom_id);
    assert_eq!(api(addr, "POST", "/api/kick", &admin, &kick).await.0, 200);
    assert!(matches!(
        console.recv(Duration::from_secs(2)).await.unwrap(),
        Received::Closed
    ));

    // Sign-in by password: a session token, ended by signing out; five failures for an email, and it waits.
    server::users::set_password(&db, "player@example.org", "long enough pass")
        .await
        .unwrap();
    let sign_in =
        |password: &str| format!(r#"{{"email":"Player@example.org","password":"{password}"}}"#);
    let (status, body) = api(
        addr,
        "POST",
        "/api/session",
        "X-None: 1",
        &sign_in("long enough pass"),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let session: shared::Session = serde_json::from_str(&body).unwrap();
    let bearer = format!("Authorization: Bearer {}", session.token);
    assert_eq!(api(addr, "GET", "/api/me", &bearer, "").await.0, 200);
    assert_eq!(api(addr, "POST", "/api/logout", &bearer, "").await.0, 200);
    assert_eq!(
        api(addr, "GET", "/api/me", &bearer, "").await.0,
        401,
        "signed out"
    );
    for _ in 0..5 {
        assert_eq!(
            api(addr, "POST", "/api/session", "X-None: 1", &sign_in("wrong"))
                .await
                .0,
            401
        );
    }
    let right = sign_in("long enough pass");
    assert_eq!(
        api(addr, "POST", "/api/session", "X-None: 1", &right)
            .await
            .0,
        429,
        "after five failures"
    );
    // In parallel, the count still holds: at most five checked, the rest refused at once.
    let mut parallel = Vec::new();
    for _ in 0..20 {
        let body = r#"{"email":"admin@example.org","password":"guess"}"#;
        parallel.push(tokio::spawn(async move {
            api(addr, "POST", "/api/session", "X-None: 1", body).await.0
        }));
    }
    let mut statuses = Vec::new();
    for p in parallel {
        statuses.push(p.await.unwrap());
    }
    assert_eq!(
        statuses.iter().filter(|&&s| s == 401).count(),
        5,
        "{statuses:?}"
    );
    let long = format!(
        r#"{{"email":"{}@example.org","password":"x"}}"#,
        "a".repeat(300)
    );
    assert_eq!(
        api(addr, "POST", "/api/session", "X-None: 1", &long)
            .await
            .0,
        400
    );

    let key = admin
        .trim_start_matches("Authorization: Bearer ")
        .split_once('.')
        .unwrap()
        .0;
    assert!(revoke(&db, key, "test").await.unwrap());
    assert_eq!(
        api(addr, "GET", "/api/me", &admin, "").await.0,
        401,
        "revoked"
    );
    running.stop().await;
}

/// A ban (`access.rs`): through the API, the player kicked and their console refused at login with a message (an
/// alert, not a drop); lifted, it logs in again. Unknown players and an empty reason are refused.
#[tokio::test]
async fn a_banned_console_is_refused_until_unbanned() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let (addr, lmp) = (running.api.unwrap(), running.lmp.unwrap());
    let db = server::database::open(&config.database_url).await.unwrap();
    server::users::add(&db, "admin@example.org", server::users::Role::Admin)
        .await
        .unwrap();
    let token = server::users::issue(&db, "admin@example.org", "cli")
        .await
        .unwrap();
    let admin = format!("Authorization: Bearer {token}");
    let (code, _, _) = first_visit(lmp, None).await;
    let mut console = play(&running, &code).await;
    let capcom_id = first_visit(lmp, Some(&code)).await.2[0].0.clone();

    let ban =
        |player: &str, reason: &str| format!(r#"{{"player":"{player}","reason":"{reason}"}}"#);
    assert_eq!(
        api(addr, "POST", "/api/ban", &admin, &ban(&capcom_id, " "))
            .await
            .0,
        400
    );
    assert_eq!(
        api(addr, "POST", "/api/ban", &admin, &ban("NOBODY", "x"))
            .await
            .0,
        404
    );
    // The same console again, between LMP and FMP: its ticket waits, it isn't online to be kicked.
    let ticket = login(lmp, &code, 1).await;
    let (status, body) = api(
        addr,
        "POST",
        "/api/ban",
        &admin,
        &ban(&capcom_id, "cheating"),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let by: Option<i64> = sqlx::query_scalar("SELECT created_by FROM access_rules")
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(by.is_some(), "the ban's author");
    let (status, body) = api(addr, "GET", "/api/bans", &admin, "").await;
    let bans: Vec<shared::BanInfo> = serde_json::from_str(&body).unwrap();
    assert_eq!(status, 200);
    assert_eq!(
        (
            bans.len(),
            bans[0].capcom_ids.clone(),
            bans[0].reason.as_str()
        ),
        (1, vec![capcom_id.clone()], "cheating")
    );
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

    let unban = format!(r#"{{"player":"{}"}}"#, capcom_id.to_lowercase());
    assert_eq!(api(addr, "POST", "/api/unban", &admin, &unban).await.0, 200);
    assert_eq!(
        api(addr, "GET", "/api/bans", &admin, "").await.1,
        "[]",
        "lifted"
    );
    assert_eq!(
        api(addr, "POST", "/api/unban", &admin, &unban).await.0,
        404,
        "twice"
    );
    match login_info(lmp, Some(&code)).await {
        Some(Received::Frame(f)) => assert_eq!(f.id, AnsLoginInfo::ID),
        other => panic!("expected a login, got {other:?}"),
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
async fn login_info(lmp: SocketAddr, support_code: Option<&str>) -> Option<Received> {
    let mut console = Console::connect(lmp, false).await.unwrap();
    expect::<ReqConnection>(&mut console).await;
    let data = connection_data(support_code, None);
    send(&mut console, 0, AnsConnection { data }).await;
    if let Received::Closed = console.recv(Duration::from_secs(5)).await.unwrap() {
        return None;
    }
    let info = ReqLoginInfo {
        data: PatData::new(),
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
    let (opn, lmp) = (running.opn, running.lmp.unwrap());
    assert!(login_info(lmp, None).await.is_none(), "LMP before OPN");
    let (mut console, status) = opn_connect(opn, b"RMHP08", NATNEG_HOSTNAMES).await;
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
        let mut c = Console::connect(lmp, false).await.unwrap();
        expect::<ReqConnection>(&mut c).await;
        open.push(c);
    }
    let mut seventeenth = Console::connect(lmp, false).await.unwrap();
    assert!(matches!(
        seventeenth.recv(Duration::from_secs(5)).await.unwrap(),
        Received::Closed
    ));
    running.stop().await;
}

/// Tier 3, decision 9 at LMP: a database failure at login is refused with a message, and the console asking again
/// keeps its own support code (it isn't handed a new one).
#[tokio::test]
async fn a_database_failure_at_login_keeps_the_code() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let lmp = running.lmp.unwrap();
    let (code, _, _) = first_visit(lmp, None).await;
    let db = sqlx::SqlitePool::connect(&config.database_url)
        .await
        .unwrap();
    let sql = |sql: &'static str| sqlx::query(sql).execute(&db);
    let mut console = Console::connect(lmp, false).await.unwrap();
    expect::<ReqConnection>(&mut console).await;
    let data = connection_data(Some(&code), None);
    send(&mut console, 0, AnsConnection { data }).await;
    assert_eq!(expect::<NtcLogin>(&mut console).await.status, 1);
    sql("ALTER TABLE consoles RENAME TO hidden").await.unwrap();
    let info = || ReqLoginInfo {
        data: PatData::new(),
    };
    send(&mut console, 1, info()).await;
    let text = expect_alert(&mut console, AnsLoginInfo::ID).await;
    assert!(text.contains("Not available right now"), "{text}");
    sql("ALTER TABLE hidden RENAME TO consoles").await.unwrap();
    send(&mut console, 2, info()).await;
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
    let (code, _, _) = first_visit(running.lmp.unwrap(), None).await;
    play(running, &code).await
}

/// A new game of `support_code`'s console, slot 1, up to a world server.
async fn play(running: &server::Running, support_code: &str) -> Console {
    let (lmp, fmp) = (running.lmp.unwrap(), running.fmp.unwrap());
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
    let (lmp, fmp) = (running.lmp.unwrap(), running.fmp.unwrap());
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
async fn new_hunter(lmp: SocketAddr) -> (String, String) {
    let (code, _, _) = first_visit(lmp, None).await;
    login(lmp, &code, 1).await;
    let (_, _, slots) = first_visit(lmp, Some(&code)).await;
    (code, slots[0].0.clone())
}

/// The message notices (`NtcTell`, `NtcFriendAccept`, `NtcFriendAdd`) a console receives within 200 ms.
async fn mail(console: &mut Console) -> Vec<wire::PacketId> {
    let kinds = [NtcTell::ID, NtcFriendAccept::ID, NtcFriendAdd::ID];
    let seen = drain(console, Duration::from_millis(200)).await;
    seen.into_iter().filter(|id| kinds.contains(id)).collect()
}

/// Tier 3: messages, with or without a friend request, are stored and delivered once, when the recipient stands in
/// a city (the game's inbox drops what arrives while a scene loads, and forgets it all when the game quits).
#[tokio::test]
async fn messages_wait_for_their_recipient() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let lmp = running.lmp.unwrap();
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
    assert_eq!(mail(&mut b).await, [NtcTell::ID, NtcFriendAccept::ID]);
    send(&mut b, 31, position()).await;
    assert_eq!(mail(&mut b).await, []);

    // B stands in the city now: a message arrives at once; a blocked hunter's never does.
    let mut c = play(&running, &c_code).await;
    send(&mut c, 10, tell(&b_id, b"hi")).await;
    assert_eq!(mail(&mut b).await, [NtcTell::ID]);
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
    assert_eq!(mail(&mut b).await, [NtcFriendAccept::ID]);
    send(&mut b, 33, request(&a_id)).await;
    assert_eq!(mail(&mut b).await, [NtcFriendAdd::ID]);
    assert_eq!(mail(&mut a).await, [NtcFriendAdd::ID]);
    running.stop().await;
}

/// Tier 3, architecture decision 9: the database failing mid-game refuses the request with a message and keeps the
/// player; what doesn't need the database goes on.
#[tokio::test]
async fn a_database_failure_keeps_the_player() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let (code, me) = new_hunter(running.lmp.unwrap()).await;
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
    let (other_code, _) = new_hunter(running.lmp.unwrap()).await;
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
    // Stored before the table goes.
    assert!(
        drain(&mut other, Duration::from_millis(200))
            .await
            .contains(&AnsTell::ID)
    );
    let position = || NtcLayerUserPosition {
        data: wire::packets::Rest(position_lp2(&[0; 37]).into()),
    };
    sql("ALTER TABLE messages RENAME TO hidden").await.unwrap();
    send(&mut console, 31, position()).await;
    assert_eq!(mail(&mut console).await, []);
    sql("ALTER TABLE hidden RENAME TO messages").await.unwrap();
    send(&mut console, 32, position()).await;
    assert_eq!(mail(&mut console).await, [NtcTell::ID]);
    running.stop().await;
}

/// Tier 3: a profile notice for one player (`target` 1, naming them) reaches only them; one for everyone (2), all.
#[tokio::test]
async fn profile_notices_reach_their_target() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let lmp = running.lmp.unwrap();
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
#[tokio::test]
async fn announcements_reach_everyone() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let mut in_world = enter_world(&running).await;
    let mut in_gate = enter_world(&running).await;
    let info = PatData::new();
    send(&mut in_gate, 10, ReqLayerDown { layer: 1, info }).await;
    drain(&mut in_gate, Duration::from_millis(100)).await;
    let health = running.health;
    let answer = tokio::task::spawn_blocking(move || {
        server::health::announce(health, "Maintenance tonight")
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
    let empty = tokio::task::spawn_blocking(move || server::health::announce(health, " "))
        .await
        .unwrap();
    assert!(empty.is_err());
    running.stop().await;
}

/// Tier 3: `mh3sp stop` warns players, refuses nothing meanwhile, reminds them, can be cancelled, and stops the server
/// at its end.
#[tokio::test]
async fn stopping_warns_and_can_be_cancelled() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let health = running.health;
    let stop = |delay: &'static str| {
        tokio::task::spawn_blocking(move || server::health::stop(health, delay))
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
        opn_login(running.opn, b"RMHE08", NATNEG_HOSTNAMES).await.0,
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
    let (status, body) = http_get(running.health, "/metrics").await;
    assert_eq!(status, "200");
    let m: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(m["players"], 2);
    assert_eq!(m["world_servers"]["Valor1"], 2);
    assert_eq!(m["natneg"]["pairs_direct"], 1);
    assert!(m["requests"]["count"].as_u64().unwrap() > 10, "{body}");
    assert!(!body.contains("capcom"), "{body}");
    // The tests build the server without the `prometheus` feature: no such endpoint.
    assert_eq!(
        http_status(running.health, "/metrics/prometheus").await,
        "404"
    );
    running.stop().await;
}

/// Tier 3, M5 runs 15–16: players in a hunt get announcements as party chat from themselves (town chat doesn't show
/// there, party chat only from a member), others as town chat; party chat a member sends reaches the others.
#[tokio::test]
async fn hunters_get_announcements() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let (mut a, mut b) = party(&running, 1, true).await;
    let mut in_world = enter_world(&running).await;
    let health = running.health;
    tokio::task::spawn_blocking(move || server::health::announce(health, "Hello"))
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
    let lmp = running.lmp.unwrap();
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

/// Every frame a console gets within `time`.
async fn frames(console: &mut Console, time: Duration) -> Vec<Frame> {
    let mut out = Vec::new();
    let end = tokio::time::Instant::now() + time;
    while let Ok(Ok(Received::Frame(f))) =
        tokio::time::timeout_at(end, console.recv(Duration::from_secs(5))).await
    {
        out.push(f);
    }
    out
}

/// Tier 3, quest bots (`experiment-bots.md`, sprint 1): a bot joins a player's party when the brain is connected, as a
/// member the other consoles see arrive in the city and the party; the brain is told; it leaves when asked, and when
/// no hunter is left in its city.
#[tokio::test]
async fn bots_join_a_party() {
    use futures_util::{SinkExt, StreamExt};
    let (mut config, _db) = config();
    config.bot_port = Some(0);
    let running = server::start(&config).await.unwrap();
    let health = running.health;
    let bot = |verb: &'static str, who: String| {
        tokio::task::spawn_blocking(move || server::health::bot(health, verb, &who))
    };
    let (mut a, mut b) = party(&running, 3, false).await;
    let player = own_id(&mut a, 30).await;
    drain(&mut b, Duration::from_millis(100)).await;

    // No brain yet: refused.
    let refused = bot("add", player.clone()).await.unwrap().unwrap_err();
    assert!(refused.to_string().contains("NoBrain"), "{refused}");
    // Nor without a party.
    let tunnel = tokio::net::TcpStream::connect(running.bots.unwrap())
        .await
        .unwrap();
    let mut brain = tokio_util::codec::Framed::new(tunnel, tunnel::codec());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(bot("add", "ZZZZZZ".into()).await.unwrap().is_err());

    // Joined: a bot's Capcom ID, the brain told which party, the others see it arrive.
    let said = bot("add", player.clone()).await.unwrap().unwrap();
    let id = said.split_whitespace().nth(1).unwrap().to_owned();
    assert!(id.starts_with('0') && id.len() == 6, "{said}");
    let created = brain.next().await.unwrap().unwrap();
    match tunnel::decode::<tunnel::ToBrain>(&created).unwrap() {
        tunnel::ToBrain::Create { bot, party } => {
            assert_eq!(
                (
                    bot.as_str(),
                    party.server,
                    party.gate,
                    party.city,
                    party.circle
                ),
                (id.as_str(), 1, 3, 1, 1)
            );
        }
        other => panic!("{other:?}"),
    }
    let seen = frames(&mut b, Duration::from_millis(300)).await;
    let names = |id: wire::PacketId| seen.iter().filter(move |f| f.id == id).count();
    assert_eq!(names(NtcLayerIn::ID), 1);
    // Ready at once.
    let ready = seen
        .iter()
        .find(|f| f.id == NtcCircleMatchOptionSet::ID)
        .unwrap();
    let ready = NtcCircleMatchOptionSet::decode(&ready.payload).unwrap();
    assert_eq!(ready.options.get(0x03), Some(&Item::Byte(1)));
    let joined = seen.iter().find(|f| f.id == NtcCircleJoin::ID).unwrap();
    let joined = NtcCircleJoin::decode(&joined.payload).unwrap();
    assert_eq!(
        (
            String::from_utf8(joined.capcom_id.to_vec()).unwrap(),
            joined.player
        ),
        (id.clone(), 3)
    );
    send(&mut b, 31, ReqLayerUserList { fields: vec![] }).await;
    assert_eq!(expect::<AnsLayerUserList>(&mut b).await.users.len(), 3);

    // A peer of the city's session: its record (kind 6) after its arrival, its port from the relay's range...
    let peer_record = |body: &mut wire::Reader| {
        body.u16().unwrap();
        let record = body.pat_data().unwrap();
        let field = |f| record.get(f).cloned();
        (field(0x01), field(0x06))
    };
    let announced = seen.iter().find(|f| f.id == NtcLayerBinary::ID).unwrap();
    let announced = NtcLayerBinary::decode(&announced.payload).unwrap();
    let mut body = wire::Reader::new(&announced.data.0);
    assert_eq!(body.lp2_string().unwrap(), id.as_bytes());
    body.pat_data().unwrap();
    match peer_record(&mut body) {
        (Some(Item::Byte(6)), Some(Item::Word(port))) => {
            assert!(config.relay_ports.contains(&port))
        }
        other => panic!("{other:?}"),
    }
    // A peer message as a console sends it: an empty sender, a length, the record.
    let peer_message = |record: &PatData| {
        let mut encoded = Vec::new();
        wire::PatWrite::put_pat_data(&mut encoded, record).unwrap();
        let mut data = Vec::new();
        wire::PatWrite::put_pat_data(&mut data, &PatData::new()).unwrap();
        data.put_u16(encoded.len() as u16);
        data.extend_from_slice(&encoded);
        Rest(data.into())
    };
    // ...and a console's record answered with kind 7, as the consoles do.
    let record = wire::PatData::new()
        .with(0x01, Item::Byte(6))
        .with(0x03, Item::Long(0x1111_2222))
        .with(0x04, Item::Long(1))
        .with(0x05, Item::Long(2))
        .with(0x06, Item::Word(3000));
    send(
        &mut b,
        32,
        NtcLayerBinary {
            data: peer_message(&record),
        },
    )
    .await;
    let answer = expect::<NtcLayerBinary2>(&mut b).await;
    assert_eq!(answer.capcom_id, id.as_bytes());
    let mut body = wire::Reader::new(&answer.data.0);
    body.pat_data().unwrap();
    assert!(matches!(peer_record(&mut body).0, Some(Item::Byte(7))));
    // A link's kind 5 to the bot, answered kind 4 as a city's host does.
    let linked = PatData::new()
        .with(0x01, Item::Byte(5))
        .with(0x02, Item::Long(0));
    send(
        &mut b,
        33,
        NtcLayerBinary2 {
            capcom_id: Bytes::copy_from_slice(id.as_bytes()),
            data: peer_message(&linked),
        },
    )
    .await;
    let answer = expect::<NtcLayerBinary2>(&mut b).await;
    let mut body = wire::Reader::new(&answer.data.0);
    body.pat_data().unwrap();
    assert!(matches!(peer_record(&mut body).0, Some(Item::Byte(4))));

    // The party's messages: the bot's on joining (kind 2), and its own after a departure when the others send theirs
    // (kind 3: each member waits for everyone's).
    let circle_kind = |f: &wire::Frame| {
        let ntc = NtcCircleBinary::decode(&f.payload).unwrap();
        let mut body = wire::Reader::new(&ntc.data.0);
        let from = body.lp2_string().unwrap();
        body.pat_data().unwrap();
        (from, peer_record(&mut body).0)
    };
    let joined = seen.iter().find(|f| f.id == NtcCircleBinary::ID).unwrap();
    assert_eq!(
        circle_kind(joined),
        (Bytes::from(id.clone()), Some(Item::Byte(2)))
    );
    let departed = PatData::new()
        .with(0x01, Item::Byte(3))
        .with(0x02, Item::Long(3));
    send(
        &mut a,
        35,
        NtcCircleBinary {
            circle: 1,
            data: peer_message(&departed),
        },
    )
    .await;
    for c in [&mut a, &mut b] {
        let got = frames(c, Duration::from_millis(200)).await;
        assert!(
            got.iter()
                .filter(|f| f.id == NtcCircleBinary::ID)
                .any(|f| circle_kind(f) == (Bytes::from(id.clone()), Some(Item::Byte(3))))
        );
    }

    // The bot connects as the pair's "to" side, also when the console's pings come before the host's announcement of
    // the pair (seen in the lab: the pings then end with NAT negotiation, before the bot knows its side).
    let mut body = wire::Reader::new(&announced.data.0);
    body.lp2_string().unwrap();
    body.pat_data().unwrap();
    body.u16().unwrap();
    let bot_record = body.pat_data().unwrap();
    let (Some(Item::Long(bot_peer)), Some(Item::Word(bot_port))) =
        (bot_record.get(0x03).cloned(), bot_record.get(0x06).cloned())
    else {
        panic!("{bot_record:?}")
    };
    let console_peer = 0x1111_2222;
    let game = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let ping = wire::natneg::Record {
        cookie: console_peer ^ bot_peer,
        body: wire::natneg::Body::Ping(wire::natneg::Peer {
            addr: "127.0.0.1:1".parse().unwrap(),
            got_your_data: 0,
            finished: 0,
        }),
    };
    game.send_to(&ping.encode(), ("127.0.0.1", bot_port))
        .await
        .unwrap();
    let mut buf = [0u8; 2048];
    let n = tokio::time::timeout(Duration::from_secs(1), game.recv(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert!(buf[..n].starts_with(&wire::natneg::MAGIC));
    let pair = PatData::new()
        .with(0x01, Item::Byte(8))
        .with(0x07, Item::Long(console_peer))
        .with(0x08, Item::Long(bot_peer))
        .with(0x09, Item::Byte(2));
    send(
        &mut b,
        34,
        NtcLayerBinary {
            data: peer_message(&pair),
        },
    )
    .await;
    let challenge = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let n = game.recv(&mut buf).await.unwrap();
            if buf[..n].starts_with(&[0xfe, 0xfe, 0x01]) {
                return buf[7..n].to_vec();
            }
        }
    })
    .await
    .expect("the bot's GT2 challenge");
    assert!(wire::p2p::gt2_valid_challenge(
        &challenge[..].try_into().unwrap()
    ));
    drain(&mut a, Duration::from_millis(100)).await;
    drain(&mut b, Duration::from_millis(100)).await;
    drain(&mut a, Duration::from_millis(100)).await;

    // Asked to leave: gone, the brain told.
    assert!(bot("remove", id.clone()).await.unwrap().is_ok());
    let left = brain.next().await.unwrap().unwrap();
    assert_eq!(
        tunnel::decode::<tunnel::ToBrain>(&left).unwrap(),
        tunnel::ToBrain::Leave { bot: id.clone() }
    );
    assert!(bot("remove", id).await.unwrap().is_err());

    // Only the leader departs: B can't.
    drain(&mut a, Duration::from_millis(100)).await;
    drain(&mut b, Duration::from_millis(100)).await;
    send(&mut b, 41, ReqCircleMatchStart {}).await;
    expect::<AnsCircleMatchStart>(&mut b).await;
    let seen = frames(&mut a, Duration::from_millis(200)).await;
    assert!(seen.iter().all(|f| f.id != NtcCircleMatchStart::ID));

    // Its party broken (the leader leaves before departure): it goes.
    bot("add", player.clone()).await.unwrap().unwrap();
    brain.next().await.unwrap().unwrap();
    send(&mut a, 42, ReqCircleLeave { circle: 1 }).await;
    let left = tokio::time::timeout(Duration::from_secs(2), brain.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        tunnel::decode::<tunnel::ToBrain>(&left).unwrap(),
        tunnel::ToBrain::Leave { .. }
    ));

    // Its quest departed, then its last hunter left it (a departed quest passes to its next member): it goes.
    send(
        &mut a,
        44,
        ReqCircleCreate {
            info: PatData::new().with(0x09, Item::Long(4)),
            extra: Default::default(),
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    bot("add", player.clone()).await.unwrap().unwrap();
    brain.next().await.unwrap().unwrap();
    send(
        &mut a,
        45,
        ReqCircleMatchOptionSet {
            options: PatData::new().with(0x03, Item::Byte(1)),
        },
    )
    .await;
    send(&mut a, 46, ReqCircleMatchStart {}).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    send(&mut a, 47, ReqCircleLeave { circle: 1 }).await;
    let left = tokio::time::timeout(Duration::from_secs(2), brain.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        tunnel::decode::<tunnel::ToBrain>(&left).unwrap(),
        tunnel::ToBrain::Leave { .. }
    ));

    // A hunter walking into the city: the bot makes room, out of the city and the party.
    send(
        &mut a,
        48,
        ReqCircleCreate {
            info: PatData::new().with(0x09, Item::Long(4)),
            extra: Default::default(),
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    bot("add", player.clone()).await.unwrap().unwrap();
    brain.next().await.unwrap().unwrap();
    let mut c = enter_world(&running).await;
    for (seq, layer) in [(10, 3), (11, 1)] {
        send(
            &mut c,
            seq,
            ReqLayerDown {
                layer,
                info: PatData::new(),
            },
        )
        .await;
    }
    let left = tokio::time::timeout(Duration::from_secs(2), brain.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        tunnel::decode::<tunnel::ToBrain>(&left).unwrap(),
        tunnel::ToBrain::Leave { .. }
    ));
    send(&mut a, 49, ReqCircleLeave { circle: 1 }).await;
    send(&mut c, 12, ReqLayerUp {}).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Left alone in the city with no hunter: it goes too.
    send(
        &mut a,
        43,
        ReqCircleCreate {
            info: PatData::new().with(0x09, Item::Long(4)),
            extra: Default::default(),
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let said = bot("add", player).await.unwrap().unwrap();
    let id = said.split_whitespace().nth(1).unwrap().to_owned();
    brain.next().await.unwrap().unwrap();
    for c in [&mut a, &mut b] {
        send(c, 40, ReqLayerUp {}).await;
    }
    let left = tokio::time::timeout(Duration::from_secs(2), brain.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        tunnel::decode::<tunnel::ToBrain>(&left).unwrap(),
        tunnel::ToBrain::Leave { bot: id }
    );
    let _ = brain.close().await;
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
/// back. Whatever they did, the server stays up and, once they're all gone, nothing is left behind.
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
    let mut c = enter_world(running).await;
    let mut dropped = 0;
    for seq in 100..100 + actions as u16 {
        let n = random(3) as u16 + 1;
        let frames: Vec<Frame> = match random(12) {
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
            _ => {
                // Vanishes, comes back.
                c = enter_world(running).await;
                continue;
            }
        };
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
    }
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
