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
        // MH3SP's event files (`scripts/ci-tests.sh` provides them).
        content_dir: std::env::var_os("CONTENT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| repo().join("../mh3sp")),
        clock_offset: 0,
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

/// When the scenario was recorded, by the reference server's clock (its first `AnsServerTime`).
fn recorded_at(scenario: &Path) -> Option<i64> {
    let recordings = corpus::load(scenario).unwrap();
    let frame = recordings
        .iter()
        .flat_map(|r| r.connections.iter().flat_map(|c| &c.frames))
        .filter(|f| f.name == "AnsServerTime")
        .min_by_key(|f| f.t)?;
    let payload = corpus::unhex(&frame.payload).unwrap();
    Some(i64::from(u32::from_be_bytes(
        payload[..4].try_into().unwrap(),
    )))
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

/// Whole scenarios of the milestone in progress (M3) that replay cleanly already; replaced by `REACHED` once M3 is.
const CLEAN_SO_FAR: [&str; 8] = [
    "s04-city-join",
    "s04-city-join-2",
    "s05-quest-party",
    "s06-disconnect",
    "s07-chat",
    "s08-friends",
    "s09-blacklist",
    "s10-party",
];

#[tokio::test]
async fn clean_so_far() {
    for name in CLEAN_SO_FAR {
        let scenario = repo().join("corpus").join(name);
        assert_clean(&scenario, &replay(&scenario).await);
    }
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

/// Not a gate yet: prints how each FMP connection of the corpus replays, while M2 is built.
#[tokio::test]
#[ignore]
async fn fmp_progress() {
    for scenario in scenarios() {
        for r in replay(&scenario)
            .await
            .iter()
            .filter(|r| r.name.ends_with("-fmp"))
        {
            let detail = r.mismatches.first().map_or(String::new(), |m| match &m.got {
                replay::Got::Frame(f) => {
                    let expected = corpus::unhex(&m.expected.payload).unwrap();
                    let at = expected.iter().zip(f.payload.iter()).position(|(a, b)| a != b).unwrap_or(expected.len().min(f.payload.len()));
                    let around = |b: &[u8]| corpus::hex(&b[at.saturating_sub(8).min(b.len())..(at + 16).min(b.len())]);
                    format!(
                        "{} (got {}): first difference at {at} of {}/{}: expected …{}… got …{}…",
                        m.expected.name, f.id, expected.len(), f.payload.len(), around(&expected), around(&f.payload)
                    )
                }
                other => format!("{} {other:?}", m.expected.name),
            });
            if std::env::var_os("FULL").is_some()
                && let Some(m) = r.mismatches.first()
                && let replay::Got::Frame(f) = &m.got
            {
                println!(
                    "EXPECTED {}\nGOT      {}",
                    m.expected.payload,
                    corpus::hex(&f.payload)
                );
            }
            println!(
                "{} {}: {}/{} {detail}",
                scenario.file_name().unwrap().to_string_lossy(),
                r.name,
                r.matched,
                r.expected
            );
        }
    }
}

/// A new console's whole way in: login, server selection (Valor1), then the world server connection.
async fn enter_world(running: &server::Running) -> Console {
    let (lmp, fmp) = (running.lmp.unwrap(), running.fmp.unwrap());
    let (code, _, _) = first_visit(lmp, None).await;
    let ticket = login(lmp, &code, 1).await;
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
    // Reserved: shown full to the other player, who can't create it.
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
    send(&mut a, 13, ReqLayerCreateFoot { number: 1, unk: 0 }).await;
    expect::<AnsLayerCreateFoot>(&mut a).await;

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

    // The creator leaves the game: B sees them go and becomes the city's host; the city stays.
    send(&mut a, 14, ReqLayerUserList { fields: vec![] }).await;
    let a_id = string(expect::<AnsLayerUserList>(&mut a).await.users[0].get(0x01));
    drop(a);
    assert_eq!(
        String::from_utf8(expect::<NtcLayerOut>(&mut b).await.capcom_id.to_vec()).unwrap(),
        a_id
    );
    let host = expect::<NtcLayerHost>(&mut b).await;
    assert_ne!(String::from_utf8(host.capcom_id.to_vec()).unwrap(), a_id);
    send(&mut b, 16, ReqLayerUserList { fields: vec![] }).await;
    assert_eq!(expect::<AnsLayerUserList>(&mut b).await.users.len(), 1);
    running.stop().await;
}

/// Tier 3: profile writes are validated; an invalid one is refused and others keep seeing the previous block.
#[tokio::test]
async fn invalid_profiles_are_refused() {
    let (config, _db) = config();
    let running = server::start(&config).await.unwrap();
    let mut a = enter_world(&running).await;
    let mut b = enter_world(&running).await;
    // HR 5, weapon type 2; then HR 1000.
    for (seq, offset, data) in [(10, 0, vec![0, 5, 0, 0, 0, 2]), (11, 0, vec![0x03, 0xe8])] {
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
    assert!(blocks.contains(&vec![0, 5, 0, 0, 0, 2]), "{blocks:?}");
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

/// Tier 3: positions relayed through one city of 4 players reach the others in under 2 ms (99th percentile, on one
/// host). MH3SP took ~50–80 ms; the console sends positions every 500 ms, so the relay must add next to nothing.
#[tokio::test]
async fn relay_latency() {
    use std::time::Instant;
    const ROUNDS: u32 = 50;
    const P99_LIMIT: Duration = Duration::from_millis(2);

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
            position[..4].copy_from_slice(&round.to_be_bytes());
            position[4] = sender as u8;
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
    assert!(pct(99) < P99_LIMIT, "p99 {:?} over {P99_LIMIT:?}", pct(99));
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
