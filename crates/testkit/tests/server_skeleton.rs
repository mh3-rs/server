//! The Rust server, started in-process on random ports, against the scripted console and the corpus.
//! Needs Postgres and Valkey: `DATABASE_URL` / `VALKEY_URL` (scripts/dev.sh locally, services in CI).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use server::Config;
use server::config::{Listen, Tls, Url};
use testkit::client::{Console, Received};
use testkit::corpus;
use testkit::masks::{Bindings, Masks};
use testkit::replay::{self, Got, Targets};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn env(key: &str) -> String {
    std::env::var(key)
        .unwrap_or_else(|_| panic!("{key} is not set: run scripts/dev.sh (or use CI)"))
}

fn config() -> Config {
    let any: SocketAddr = "127.0.0.1:0".parse().unwrap();
    Config {
        listen: Listen {
            opn: Some(any),
            lmp: Some(any),
            fmp: Some(any),
            health: any,
        },
        tls: Tls {
            cert: repo().join("certs/dev/server.crt"),
            key: repo().join("certs/dev/server.key"),
        },
        database: Url {
            url: env("DATABASE_URL"),
        },
        valkey: Url {
            url: env("VALKEY_URL"),
        },
    }
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

#[tokio::test]
async fn accepts_wii_tls_and_reports_health() {
    let running = server::start(&config()).await.unwrap();

    // OPN: the scripted console speaks TLS like a Wii (TLS 1.0, AES128-SHA).
    let mut opn = Console::connect(running.opn.unwrap(), true).await.unwrap();
    let ping = wire::Frame {
        seq: 1,
        id: wire::PacketId(0x6001_0200),
        payload: bytes::Bytes::new(),
    };
    opn.send(ping).await.unwrap();
    // M0 doesn't answer anything yet.
    let silence = opn
        .recv(std::time::Duration::from_millis(300))
        .await
        .unwrap();
    assert!(matches!(silence, Received::Timeout));
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

/// Tier 2: scenarios whose milestone is reached must replay cleanly against the Rust server.
#[tokio::test]
async fn reached_scenarios_replay_cleanly() {
    let corpus_dir = repo().join("corpus");
    let masks = Masks::load(&corpus_dir.join("masks.toml")).unwrap();
    let mut scenarios: Vec<_> = std::fs::read_dir(&corpus_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.join("raw").is_dir())
        .collect();
    scenarios.sort();
    for scenario in scenarios {
        let milestone = corpus::milestone(&scenario).unwrap();
        if milestone.as_str() > corpus::REACHED {
            continue;
        }
        // A fresh server per scenario, as when it was recorded.
        let running = server::start(&config()).await.unwrap();
        let targets = Targets {
            opn: running.opn.unwrap(),
            lmp: running.lmp.unwrap(),
            fmp: running.fmp.unwrap(),
            nudge: false,
        };
        let reports = replay::replay(&scenario, &targets, &masks).await.unwrap();
        running.stop().await;
        let dirty: Vec<_> = reports
            .iter()
            .filter(|r| !r.is_clean())
            .map(|r| &r.name)
            .collect();
        assert!(dirty.is_empty(), "{}: {dirty:?}", scenario.display());
    }
}

/// The harness reaches the skeleton: the first expected frame of a real recording is awaited
/// over a real TLS connection, and missing (the skeleton doesn't answer yet).
#[tokio::test]
async fn replay_reaches_the_skeleton() {
    let running = server::start(&config()).await.unwrap();
    let scenario = repo().join("corpus/s01-terms-decline");
    let recording = corpus::load(&scenario).unwrap();
    let opn = &recording[0].connections[0];
    let report = replay::replay_connection(
        "dev3-01-opn".into(),
        running.opn.unwrap(),
        true,
        false,
        &opn.frames,
        &Masks::default(),
        &mut Bindings::default(),
    )
    .await;
    running.stop().await;
    assert_eq!(report.matched, 0);
    assert!(
        matches!(report.mismatches[0].got, Got::Timeout),
        "{report:?}"
    );
    assert_eq!(report.mismatches[0].expected.name, "ReqMemoryCheck");
}
