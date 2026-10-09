//! Replays a recorded scenario against a live server and compares its answers with the recording.
//!
//! - Consoles run concurrently, each one's connections in recorded order.
//! - Console frames are sent in the scenario's recorded global order: a console sends its next frame
//!   only once every frame any console sent before it (by recorded time) has been sent, so "dev2
//!   joins after dev1 created the city" holds, and so does whatever triggers a broadcast.
//! - The server frames expected between two console frames may arrive in any order: town relays
//!   follow the players' movements, not the protocol.
//! - A server frame recorded at time t is only awaited once every console frame recorded before t
//!   (its possible causes, from any console) has been sent; the receive timeout starts then. So
//!   dev1 waiting in its city for dev2 doesn't time out while dev2 is still logging in. The timeout
//!   is then [`RECV_TIMEOUT`] plus however long the frame took after its last cause in the recording
//!   (Python only tells others about a vanished player at its next line check, ~30 s later).
//! - An answer to a request this console already sent may arrive before frames recorded ahead of it (the Rust
//!   server answers at once; MH3SP was slow enough for other consoles' relays to come first): it's accepted
//!   early, and counted where the recording has it. Consoles still wait for every frame recorded before their
//!   next one, relays included, so they never overtake what caused them.
//! - Extra notifications (`Ntc…` frames matching nothing expected, and no expected frame with the
//!   same id) are listed, not failed: one that races with a player leaving may legitimately arrive or
//!   not. Missing or wrong frames fail.
//! - Line checks are time-driven: recorded ones are skipped, live ones answered on the fly.
//! - Volatile values are handled with [`crate::masks`]; bindings are shared by every connection and
//!   console of the scenario.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::pin::pin;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;
use bytes::Bytes;
use futures_util::future::join_all;
use tokio::sync::Notify;
use wire::{Frame, PacketId};

use crate::client::{Console, Received};
use crate::corpus::{self, ConsoleRecording, Dir, FrameRecord};
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
    /// Index of the expected frame in the connection's recording (line checks excluded).
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
    /// Notifications received that the recording doesn't have (tolerated, see the module doc).
    pub extras: Vec<Frame>,
    /// Set when the replay of this connection stopped early.
    pub aborted: Option<String>,
}

impl ConnectionReport {
    pub fn is_clean(&self) -> bool {
        self.mismatches.is_empty() && self.aborted.is_none()
    }
}

/// The global send order: turn `g` may go once turns `0..g` are done.
struct Turns {
    done: Mutex<Vec<bool>>,
    notify: Notify,
}

impl Turns {
    fn new(n: usize) -> Self {
        Self {
            done: Mutex::new(vec![false; n]),
            notify: Notify::new(),
        }
    }

    async fn wait(&self, turn: usize) {
        loop {
            let mut notified = pin!(self.notify.notified());
            notified.as_mut().enable();
            if self.done.lock().unwrap()[..turn].iter().all(|&d| d) {
                return;
            }
            notified.await;
        }
    }

    fn finish(&self, turns: impl IntoIterator<Item = usize>) {
        let mut done = self.done.lock().unwrap();
        for t in turns {
            done[t] = true;
        }
        drop(done);
        self.notify.notify_waiters();
    }
}

/// What every connection of a scenario shares.
struct Shared<'a> {
    /// The scenario's name (its folder), for the declared omissions.
    scenario: String,
    targets: &'a Targets,
    masks: &'a Masks,
    bindings: Mutex<Bindings>,
    turns: Turns,
}

/// One frame to replay: a console frame has its turn; a server frame, the number of turns that must
/// be done before it can be expected, and how long it took after the last of them when recorded.
struct Step<'a> {
    record: &'a FrameRecord,
    turn: Option<usize>,
    after: usize,
    lag: Duration,
}

/// Replays every connection of a scenario, consoles concurrently.
pub async fn replay(
    scenario: &Path,
    targets: &Targets,
    masks: &Masks,
) -> Result<Vec<ConnectionReport>> {
    let recordings = corpus::load(scenario)?;

    // Number console frames in recorded time order: (console, connection, frame) -> turn.
    let mut sends: Vec<(u64, usize, usize, usize)> = Vec::new();
    for (ci, rec) in recordings.iter().enumerate() {
        for (n, c) in rec.connections.iter().enumerate() {
            for (fi, f) in c.frames.iter().enumerate() {
                if f.dir == Dir::C2s && !is_line_check(f) {
                    sends.push((f.t, ci, n, fi));
                }
            }
        }
    }
    sends.sort();
    let send_times: Vec<u64> = sends.iter().map(|s| s.0).collect();
    let turn_of: HashMap<(usize, usize, usize), usize> = sends
        .iter()
        .enumerate()
        .map(|(g, &(_, ci, n, fi))| ((ci, n, fi), g))
        .collect();

    let shared = Shared {
        scenario: scenario
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        targets,
        masks,
        bindings: Mutex::new(Bindings::default()),
        turns: Turns::new(sends.len()),
    };
    let consoles = recordings
        .iter()
        .enumerate()
        .map(|(ci, rec)| replay_console(ci, rec, &turn_of, &send_times, &shared));
    Ok(join_all(consoles).await.into_iter().flatten().collect())
}

async fn replay_console(
    ci: usize,
    rec: &ConsoleRecording,
    turn_of: &HashMap<(usize, usize, usize), usize>,
    send_times: &[u64],
    shared: &Shared<'_>,
) -> Vec<ConnectionReport> {
    let mut reports = Vec::new();
    for (n, c) in rec.connections.iter().enumerate() {
        let steps: Vec<Step> = c
            .frames
            .iter()
            .enumerate()
            .filter(|(_, f)| !is_line_check(f))
            // Frames the Rust server leaves out on purpose (`[[omit]]` in divergences.toml).
            .filter(|(_, f)| {
                let name = format!("{}-{:02}-{}", rec.console, n + 1, c.role);
                !(f.dir == Dir::S2c
                    && shared
                        .masks
                        .omitted(&shared.scenario, &name, &f.name, f.seq))
            })
            .map(|(fi, record)| Step {
                record,
                turn: turn_of.get(&(ci, n, fi)).copied(),
                after: send_times.partition_point(|&t| t < record.t),
                lag: lag(send_times, record.t),
            })
            .collect();
        let name = format!("{}-{:02}-{}", rec.console, n + 1, c.role);
        let mut report = ConnectionReport {
            name,
            expected: steps.iter().filter(|s| s.record.dir == Dir::S2c).count(),
            matched: 0,
            mismatches: Vec::new(),
            extras: Vec::new(),
            aborted: None,
        };
        let outcome = match shared.targets.of(c.role) {
            Some(addr) => run(&steps, addr, c.role == "opn", shared, &mut report).await,
            None => Err(anyhow::anyhow!("no target for role {}", c.role)),
        };
        if let Err(e) = outcome {
            report.aborted = Some(format!("{e:#}"));
        }
        // Whatever happened, this connection's remaining turns must not block the others.
        shared.turns.finish(steps.iter().filter_map(|s| s.turn));
        reports.push(report);
    }
    reports
}

/// Replays one connection without the shared state of a scenario (tests, single connections).
pub async fn replay_connection(
    name: String,
    addr: SocketAddr,
    tls: bool,
    nudge: bool,
    frames: &[FrameRecord],
    masks: &Masks,
    bindings: &mut Bindings,
) -> ConnectionReport {
    let targets = Targets {
        nudge,
        ..Targets::host(addr.ip())
    };
    let shared = Shared {
        scenario: String::new(),
        targets: &targets,
        masks,
        bindings: Mutex::new(std::mem::take(bindings)),
        turns: Turns::new(0),
    };
    let steps: Vec<Step> = frames
        .iter()
        .filter(|f| !is_line_check(f))
        .map(|record| Step {
            record,
            turn: None,
            after: 0,
            lag: Duration::ZERO,
        })
        .collect();
    let mut report = ConnectionReport {
        name,
        expected: steps.iter().filter(|s| s.record.dir == Dir::S2c).count(),
        matched: 0,
        mismatches: Vec::new(),
        extras: Vec::new(),
        aborted: None,
    };
    if let Err(e) = run(&steps, addr, tls, &shared, &mut report).await {
        report.aborted = Some(format!("{e:#}"));
    }
    *bindings = shared.bindings.into_inner().unwrap();
    report
}

async fn run(
    steps: &[Step<'_>],
    addr: SocketAddr,
    tls: bool,
    shared: &Shared<'_>,
    report: &mut ConnectionReport,
) -> Result<()> {
    let mut console = Console::connect(addr, tls).await?;
    // Answers received before their recorded place (indices into `steps`).
    let mut early: Vec<usize> = Vec::new();
    // Request numbers this console has sent so far.
    let mut sent: Vec<u16> = Vec::new();
    let mut i = 0;
    while i < steps.len() {
        let step = &steps[i];
        if step.record.dir == Dir::C2s {
            if let Some(turn) = step.turn {
                wait_turns(&mut console, turn, shared).await?;
            }
            let mut payload = corpus::unhex(&step.record.payload)?;
            shared.bindings.lock().unwrap().apply(&mut payload);
            let frame = Frame {
                seq: step.record.seq,
                id: PacketId(id_of(step.record)),
                payload: Bytes::from(payload),
            };
            console.send(frame).await?;
            sent.push(step.record.seq);
            if let Some(turn) = step.turn {
                shared.turns.finish([turn]);
            }
            i += 1;
            continue;
        }

        // The server frames expected before the next console frame, in any order.
        let end = steps[i..]
            .iter()
            .position(|s| s.record.dir == Dir::C2s)
            .map_or(steps.len(), |p| i + p);
        let mut pending: Vec<usize> = (i..end).filter(|p| !early.contains(p)).collect();
        // Their causes first: frames arriving meanwhile wait in the socket.
        let after = pending.iter().map(|&p| steps[p].after).max().unwrap_or(0);
        wait_turns(&mut console, after, shared).await?;
        while !pending.is_empty() {
            let lag = pending
                .iter()
                .map(|&p| steps[p].lag)
                .max()
                .unwrap_or_default();
            let got = receive(&mut console, shared.targets.nudge, RECV_TIMEOUT + lag).await?;
            let Got::Frame(frame) = got else {
                for &p in &pending {
                    let got = match got {
                        Got::Closed => Got::Closed,
                        _ => Got::Timeout,
                    };
                    report.mismatches.push(mismatch(p, steps, got));
                }
                report.aborted = Some("server stopped answering".into());
                return Ok(());
            };
            let fits = |p: usize| {
                let record = steps[p].record;
                frame.id.0 == id_of(record)
                    && frame.seq == record.seq
                    && corpus::unhex(&record.payload).is_ok_and(|recorded| {
                        let mut bindings = shared.bindings.lock().unwrap();
                        masks::matches(
                            shared.masks,
                            &mut bindings,
                            &record.name,
                            &recorded,
                            &frame.payload,
                        )
                    })
            };
            let found = pending.iter().position(|&p| fits(p));
            // An answer recorded later, to a request already sent: early, not wrong.
            if found.is_none()
                && let Some(p) = (end..steps.len()).find(|&p| {
                    steps[p].record.dir == Dir::S2c
                        && sent.contains(&steps[p].record.seq)
                        && !early.contains(&p)
                        && fits(p)
                })
            {
                early.push(p);
                report.matched += 1;
                continue;
            }
            match found {
                Some(k) => {
                    pending.remove(k);
                    report.matched += 1;
                }
                // An unrecorded notification is tolerated, unless one with the same id is expected:
                // then it's a real difference.
                None if frame.id.kind() == Some(wire::Kind::Ntc)
                    && !pending
                        .iter()
                        .any(|&p| id_of(steps[p].record) == frame.id.0) =>
                {
                    report.extras.push(frame)
                }
                None => {
                    let p = pending.remove(0);
                    report
                        .mismatches
                        .push(mismatch(p, steps, Got::Frame(frame)));
                }
            }
        }
        i = end;
    }
    Ok(())
}

/// Waits until turns `0..turn` are done. With [`Targets::nudge`], keeps nudging this connection
/// meanwhile: a frame it sent may be stuck in Python's buffer, and another console may be waiting
/// for its effect.
async fn wait_turns(console: &mut Console, turn: usize, shared: &Shared<'_>) -> Result<()> {
    if !shared.targets.nudge {
        shared.turns.wait(turn).await;
        return Ok(());
    }
    let mut done = pin!(shared.turns.wait(turn));
    loop {
        tokio::select! {
            _ = &mut done => return Ok(()),
            _ = tokio::time::sleep(STALL) => console.send(empty(ANS_LINE_CHECK, 0)).await?,
        }
    }
}

/// The next server frame, answering live line checks and nudging a stalled server when asked.
async fn receive(console: &mut Console, nudge: bool, timeout: Duration) -> Result<Got> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let wait = if nudge { left.min(STALL) } else { left };
        match console.recv(wait).await? {
            Received::Timeout if nudge && !left.is_zero() => {
                console.send(empty(ANS_LINE_CHECK, 0)).await?;
            }
            Received::Frame(f) if f.id.0 == REQ_LINE_CHECK => {
                console.send(empty(ANS_LINE_CHECK, f.seq)).await?;
            }
            Received::Frame(f) => return Ok(Got::Frame(f)),
            Received::Closed => return Ok(Got::Closed),
            Received::Timeout => return Ok(Got::Timeout),
        }
    }
}

/// How long after the last console frame sent before `t` a server frame came, in the recording.
fn lag(send_times: &[u64], t: u64) -> Duration {
    let last = send_times
        .partition_point(|&s| s < t)
        .checked_sub(1)
        .map(|i| send_times[i]);
    Duration::from_micros(last.map_or(0, |last| t - last))
}

fn mismatch(index: usize, steps: &[Step], got: Got) -> Mismatch {
    Mismatch {
        index,
        expected: steps[index].record.clone(),
        got,
    }
}

fn empty(id: u32, seq: u16) -> Frame {
    Frame {
        seq,
        id: PacketId(id),
        payload: Bytes::new(),
    }
}

fn is_line_check(record: &FrameRecord) -> bool {
    matches!(id_of(record), REQ_LINE_CHECK | ANS_LINE_CHECK)
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
            record(Dir::S2c, 0x6002_0200, 0, "01"),       // AnsServerTime
        ];
        let addr = fake_server(vec![
            Some(frame(0x6020_0100, 0, &[0, 0, 0, 0])),
            None,                                 // AnsConnection
            Some(frame(0x6001_0100, 0, &[])),     // live line check
            None,                                 // AnsLineCheck from the harness
            Some(frame(0x6021_1000, 0, &[0x05])), // NtcLogin
            Some(frame(0x6002_0200, 0, &[0x02])), // AnsServerTime with another value
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
        assert_eq!((report.expected, report.matched), (3, 2));
        assert_eq!(report.mismatches.len(), 1);
        assert_eq!(report.mismatches[0].expected.name, "AnsServerTime");
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
