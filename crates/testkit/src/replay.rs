//! Replays a recorded scenario against a live server and compares its answers with the recording.
//!
//! Each connection is replayed in recorded order: recorded console frames are sent, recorded server
//! frames are awaited and compared. Line checks are time-driven, so recorded ones are skipped and
//! live ones are answered on the fly. Volatile values are handled with [`crate::masks`]: bindings
//! learned from server frames are shared by every connection and console of the scenario.

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use bytes::Bytes;
use wire::{Frame, PacketId};

use crate::client::{Console, Received};
use crate::corpus::{self, Dir, FrameRecord};
use crate::masks::{self, Bindings, Masks};

const REQ_LINE_CHECK: u32 = 0x6001_0100;
const ANS_LINE_CHECK: u32 = 0x6001_0200;
/// How long to wait for an expected server frame. The server waits ~1.5 s before its first frame.
pub const RECV_TIMEOUT: Duration = Duration::from_secs(5);
/// With [`Targets::nudge`], how long a wait may stall before the harness sends a nudge.
const STALL: Duration = Duration::from_millis(500);

/// Where each server role listens.
#[derive(Clone, Debug)]
pub struct Targets {
    pub opn: SocketAddr,
    pub lmp: SocketAddr,
    pub fmp: SocketAddr,
    /// Python MH3SP only: it reads one frame per socket readiness event from a buffered reader,
    /// so frames that arrive together stay stuck in its buffer until more data comes in. When a
    /// wait stalls, send a harmless `AnsLineCheck` to wake it up. Never used against our server.
    pub nudge: bool,
}

impl Targets {
    /// The standard ports (8200, 8201, 8202) on one host.
    pub fn host(ip: std::net::IpAddr) -> Self {
        Self {
            opn: SocketAddr::new(ip, 8200),
            lmp: SocketAddr::new(ip, 8201),
            fmp: SocketAddr::new(ip, 8202),
            nudge: false,
        }
    }

    fn of(&self, role: &str) -> Option<SocketAddr> {
        match role {
            "opn" => Some(self.opn),
            "lmp" => Some(self.lmp),
            "fmp" => Some(self.fmp),
            _ => None,
        }
    }
}

/// What went wrong on one expected server frame.
#[derive(Debug)]
pub struct Mismatch {
    /// Index of the expected frame in the connection's recording.
    pub index: usize,
    pub expected: FrameRecord,
    pub got: Got,
}

#[derive(Debug)]
pub enum Got {
    Frame(Frame),
    Closed,
    Timeout,
}

#[derive(Debug)]
pub struct ConnectionReport {
    /// e.g. `dev1-02-lmp`.
    pub name: String,
    /// Expected server frames.
    pub expected: usize,
    pub matched: usize,
    pub mismatches: Vec<Mismatch>,
    /// Set when the replay of this connection stopped early.
    pub aborted: Option<String>,
}

impl ConnectionReport {
    pub fn is_clean(&self) -> bool {
        self.mismatches.is_empty() && self.aborted.is_none()
    }
}

/// Replays every connection of a scenario. Consoles run one after the other, each console's
/// connections in recorded order (cross-console ordering comes with multi-console scenarios).
pub async fn replay(
    scenario: &Path,
    targets: &Targets,
    masks: &Masks,
) -> Result<Vec<ConnectionReport>> {
    let mut reports = Vec::new();
    let mut bindings = Bindings::default();
    for rec in corpus::load(scenario)? {
        for (n, c) in rec.connections.iter().enumerate() {
            let name = format!("{}-{:02}-{}", rec.console, n + 1, c.role);
            let report = match targets.of(c.role) {
                Some(addr) => {
                    let tls = c.role == "opn";
                    let nudge = targets.nudge;
                    replay_connection(name, addr, tls, nudge, &c.frames, masks, &mut bindings).await
                }
                None => ConnectionReport {
                    name,
                    expected: 0,
                    matched: 0,
                    mismatches: Vec::new(),
                    aborted: Some(format!("no target for role {}", c.role)),
                },
            };
            reports.push(report);
        }
    }
    Ok(reports)
}

pub async fn replay_connection(
    name: String,
    addr: SocketAddr,
    tls: bool,
    nudge: bool,
    frames: &[FrameRecord],
    masks: &Masks,
    bindings: &mut Bindings,
) -> ConnectionReport {
    let frames: Vec<_> = frames
        .iter()
        .filter(|f| !matches!(id_of(f), REQ_LINE_CHECK | ANS_LINE_CHECK))
        .collect();
    let mut report = ConnectionReport {
        name,
        expected: frames.iter().filter(|f| f.dir == Dir::S2c).count(),
        matched: 0,
        mismatches: Vec::new(),
        aborted: None,
    };
    if let Err(e) = run(&frames, addr, tls, nudge, masks, bindings, &mut report).await {
        report.aborted = Some(format!("{e:#}"));
    }
    report
}

async fn run(
    frames: &[&FrameRecord],
    addr: SocketAddr,
    tls: bool,
    nudge: bool,
    masks: &Masks,
    bindings: &mut Bindings,
    report: &mut ConnectionReport,
) -> Result<()> {
    let mut console = Console::connect(addr, tls).await?;
    for (index, &record) in frames.iter().enumerate() {
        if record.dir == Dir::C2s {
            let mut payload = corpus::unhex(&record.payload)?;
            bindings.apply(&mut payload);
            let frame = Frame {
                seq: record.seq,
                id: PacketId(id_of(record)),
                payload: Bytes::from(payload),
            };
            console.send(frame).await?;
            continue;
        }
        let deadline = tokio::time::Instant::now() + RECV_TIMEOUT;
        let got = loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let wait = if nudge { left.min(STALL) } else { left };
            match console.recv(wait).await? {
                Received::Timeout if nudge && !left.is_zero() => {
                    let wake = Frame {
                        seq: 0,
                        id: PacketId(ANS_LINE_CHECK),
                        payload: Bytes::new(),
                    };
                    console.send(wake).await?;
                }
                Received::Frame(f) if f.id.0 == REQ_LINE_CHECK => {
                    let answer = Frame {
                        seq: f.seq,
                        id: PacketId(ANS_LINE_CHECK),
                        payload: Bytes::new(),
                    };
                    console.send(answer).await?;
                }
                Received::Frame(f) => break Got::Frame(f),
                Received::Closed => break Got::Closed,
                Received::Timeout => break Got::Timeout,
            }
        };
        let stop = !matches!(got, Got::Frame(_));
        let recorded = corpus::unhex(&record.payload)?;
        let same = match &got {
            Got::Frame(f) => {
                f.id.0 == id_of(record)
                    && f.seq == record.seq
                    && masks::matches(masks, bindings, &record.name, &recorded, &f.payload)
            }
            _ => false,
        };
        if same {
            report.matched += 1;
        } else {
            report.mismatches.push(Mismatch {
                index,
                expected: record.clone(),
                got,
            });
        }
        if stop {
            report.aborted = Some("server stopped answering".into());
            break;
        }
    }
    Ok(())
}

fn id_of(record: &FrameRecord) -> u32 {
    u32::from_str_radix(&record.id, 16).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_util::codec::Framed;
    use wire::FrameCodec;

    fn record(dir: Dir, id: u32, seq: u16, payload: &str) -> FrameRecord {
        FrameRecord {
            t: 0,
            dir,
            seq,
            id: format!("{id:08x}"),
            name: PacketId(id).name().unwrap_or("Unknown").into(),
            payload: payload.into(),
        }
    }

    /// A fake server that sends `script` frames in order, each after reading one frame when the
    /// previous step expects it (`None` = read one client frame).
    async fn fake_server(script: Vec<Option<Frame>>) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut framed = Framed::new(socket, FrameCodec);
            for step in script {
                match step {
                    Some(frame) => framed.send(frame).await.unwrap(),
                    None => {
                        framed.next().await;
                    }
                }
            }
        });
        addr
    }

    fn frame(id: u32, seq: u16, payload: &[u8]) -> Frame {
        Frame {
            seq,
            id: PacketId(id),
            payload: Bytes::copy_from_slice(payload),
        }
    }

    #[tokio::test]
    async fn matches_answers_line_checks_and_reports_diffs() {
        let recording = [
            record(Dir::S2c, 0x6020_0100, 0, "00000000"), // ReqConnection
            record(Dir::C2s, 0x6020_0200, 0, "00"),       // AnsConnection
            record(Dir::S2c, 0x6001_0100, 0, ""),         // recorded line check: skipped
            record(Dir::S2c, 0x6021_1000, 0, "05"),       // NtcLogin
        ];
        let addr = fake_server(vec![
            Some(frame(0x6020_0100, 0, &[0, 0, 0, 0])),
            None,                                 // AnsConnection
            Some(frame(0x6001_0100, 0, &[])),     // live line check
            None,                                 // AnsLineCheck from the harness
            Some(frame(0x6021_1000, 0, &[0x03])), // NtcLogin with another status
        ])
        .await;
        let report = replay_connection(
            "dev1-01-lmp".into(),
            addr,
            false,
            false,
            &recording,
            &Masks::default(),
            &mut Bindings::default(),
        )
        .await;
        assert_eq!((report.expected, report.matched), (2, 1));
        assert_eq!(report.mismatches.len(), 1);
        assert_eq!(report.mismatches[0].expected.name, "NtcLogin");
        assert!(report.aborted.is_none());
    }

    #[tokio::test]
    async fn stops_when_the_server_closes() {
        let recording = [record(Dir::S2c, 0x6020_0100, 0, "00000000")];
        let addr = fake_server(vec![]).await;
        let report = replay_connection(
            "dev1-01-lmp".into(),
            addr,
            false,
            false,
            &recording,
            &Masks::default(),
            &mut Bindings::default(),
        )
        .await;
        assert!(matches!(report.mismatches[0].got, Got::Closed));
        assert!(report.aborted.is_some());
    }
}
