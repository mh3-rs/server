//! The Rust server, started in-process on random ports, against the scripted console and the corpus.
//! Each server gets its own throw-away SQLite file.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bytes::Bytes;
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
        tls_cert: repo().join("certs/dev/server.crt"),
        tls_key: repo().join("certs/dev/server.key"),
        database_url: format!("sqlite://{}", dir.path().join("mh3sp.db").display()),
        // Where the recordings were made: the addresses handed out match theirs.
        external_address: Some("192.168.110.150".into()),
        natneg_hostname: Some("natneg1.gs.nintendowifi.net".into()),
        secret: Some(server::config::Secret(b"test secret".to_vec())),
        maintenance: None,
    };
    (config, dir)
}

async fn http_status(addr: SocketAddr, path: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.0\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    out.split_whitespace().nth(1).unwrap_or_default().to_owned()
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
    if status != 2 {
        return (status, None);
    }
    send(&mut console, 1, ReqMaintenance {}).await;
    let text = expect::<AnsMaintenance>(&mut console).await.text;
    (status, Some(String::from_utf8(text.to_vec()).unwrap()))
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

async fn replay(scenario: &Path) -> Vec<ConnectionReport> {
    let masks = Masks::load(&repo().join("corpus")).unwrap();
    // A fresh server per scenario, as when it was recorded.
    let (config, _db) = config();
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

/// The replay gate of M1: the login connections (OPN and LMP) of every scenario, whatever its milestone.
#[tokio::test]
async fn login_connections_replay_cleanly() {
    for scenario in scenarios() {
        let reports = replay(&scenario).await;
        let login: Vec<_> = reports
            .into_iter()
            .filter(|r| r.name.ends_with("-opn") || r.name.ends_with("-lmp"))
            .collect();
        assert!(!login.is_empty());
        assert_clean(&scenario, &login);
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

/// A PAL console's connection data: its support code and its ticket, when it has them.
fn connection_data(support_code: Option<&str>, ticket: Option<&str>) -> PatData {
    let mut data = PatData::new().with(0x07, Item::String(Bytes::from_static(b"1002121503")));
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
async fn first_visit(lmp: SocketAddr, support_code: Option<&str>) -> (String, String, Vec<String>) {
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
        slots.iter().map(|u| string(u.get(0x02))).collect(),
    )
}

/// LMP's second visit: plays `slot` as `name`, returns its Capcom ID.
async fn second_visit(lmp: SocketAddr, ticket: &str, slot: u32, name: &str) -> String {
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
    let user = PatData::new().with(0x03, Item::String(name.to_owned().into()));
    send(&mut console, 1, ReqUserObject { unk: 1, slot, user }).await;
    string(expect::<AnsUserObject>(&mut console).await.user.get(0x02))
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
    assert_eq!(slots, vec!["******"; 6]);
    let capcom_id = second_visit(lmp, &ticket, 2, "Aiko").await;
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
    assert_ne!(second_visit(lmp, &ticket, 2, "Aiko").await, capcom_id);
    running.stop().await;

    // After a restart, the console finds its hunter in the same slot, with the same Capcom ID.
    let running = server::start(&config).await.unwrap();
    let lmp = running.lmp.unwrap();
    let (again, ticket, slots) = first_visit(lmp, Some(&code)).await;
    assert_eq!(again, code);
    assert_eq!(slots[1], capcom_id);
    assert_eq!(second_visit(lmp, &ticket, 2, "Aiko").await, capcom_id);
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
