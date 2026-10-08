//! The corpus: recorded game sessions, one directory per scenario.
//!
//! ```text
//! corpus/<scenario>/
//! ├── scenario.toml
//! ├── raw/<console>.pcap                         filtered Dolphin capture (the source of truth)
//! ├── tcp/<console>-<nn>-<opn|lmp|fmp|rfp>.jsonl one PAT frame per line, per connection
//! └── udp/<console>.jsonl                         NAT negotiation and P2P datagrams
//! ```
//!
//! `tcp/` and `udp/` are generated from `raw/` and committed so they can be read and diffed.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::net::SocketAddrV4;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use bytes::BytesMut;
use serde::{Deserialize, Serialize};
use tokio_util::codec::Decoder;
use wire::FrameCodec;

use crate::pcap::{self, Packet, Proto};

/// Server ports of the Capcom servers: OPN, LMP, FMP, RFP.
const GAME_PORTS: [(u16, &str); 4] = [(8200, "opn"), (8201, "lmp"), (8202, "fmp"), (8203, "rfp")];
const DNS_PORT: u16 = 53;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dir {
    /// Console to server.
    C2s,
    /// Server to console.
    S2c,
}

/// One PAT frame of a TCP connection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameRecord {
    /// Microseconds since the start of the scenario (shared by all consoles).
    pub t: u64,
    pub dir: Dir,
    pub seq: u16,
    /// Packet id, 8 hex digits.
    pub id: String,
    pub name: String,
    /// Hex.
    pub payload: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UdpDir {
    Out,
    In,
}

/// One UDP datagram, seen from the console.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Datagram {
    pub t: u64,
    pub dir: UdpDir,
    /// The console's port.
    pub local: u16,
    pub remote: String,
    /// Hex.
    pub payload: String,
}

/// A TCP connection between one console and one server role.
#[derive(Debug)]
pub struct Connection {
    pub role: &'static str,
    pub client_port: u16,
    pub frames: Vec<FrameRecord>,
    /// The raw bytes of each direction, `[c2s, s2c]`, exactly as captured.
    pub streams: [Vec<u8>; 2],
}

/// Everything recorded for one console in a scenario.
#[derive(Debug)]
pub struct ConsoleRecording {
    pub console: String,
    /// In order of first packet.
    pub connections: Vec<Connection>,
    pub datagrams: Vec<Datagram>,
}

fn role_of(port: u16) -> Option<&'static str> {
    GAME_PORTS.iter().find(|(p, _)| *p == port).map(|(_, r)| *r)
}

/// What the corpus keeps from a capture: game-server TCP and non-DNS UDP. The rest (Nintendo
/// login over HTTP, GameSpy profile TCP) identifies the console and is dropped.
fn keep(p: &Packet) -> bool {
    match p.proto {
        Proto::Tcp => role_of(p.src.port()).is_some() || role_of(p.dst.port()).is_some(),
        Proto::Udp => p.src.port() != DNS_PORT && p.dst.port() != DNS_PORT,
    }
}

/// Splits one console's packets into PAT connections and datagrams. `t0` is the scenario start.
pub fn split(console: &str, packets: &[Packet], t0: u64) -> Result<ConsoleRecording> {
    let mut connections: Vec<Connection> = Vec::new();
    let mut buffers: Vec<[BytesMut; 2]> = Vec::new();
    let mut index: HashMap<(u16, u16), usize> = HashMap::new();
    let mut datagrams = Vec::new();

    for p in packets.iter().filter(|p| keep(p)) {
        let t = p.ts_us - t0;
        match p.proto {
            Proto::Tcp => {
                let (dir, client, server) = match role_of(p.dst.port()) {
                    Some(_) => (Dir::C2s, p.src, p.dst),
                    None => (Dir::S2c, p.dst, p.src),
                };
                let i = *index
                    .entry((client.port(), server.port()))
                    .or_insert_with(|| {
                        connections.push(Connection {
                            role: role_of(server.port()).unwrap(),
                            client_port: client.port(),
                            frames: Vec::new(),
                            streams: [Vec::new(), Vec::new()],
                        });
                        buffers.push([BytesMut::new(), BytesMut::new()]);
                        connections.len() - 1
                    });
                let d = dir as usize;
                connections[i].streams[d].extend_from_slice(p.payload);
                buffers[i][d].extend_from_slice(p.payload);
                while let Some(frame) = FrameCodec.decode(&mut buffers[i][d])? {
                    connections[i].frames.push(FrameRecord {
                        t,
                        dir,
                        seq: frame.seq,
                        id: format!("{:08x}", frame.id.0),
                        name: frame.id.name().unwrap_or("Unknown").to_owned(),
                        payload: hex(&frame.payload),
                    });
                }
            }
            Proto::Udp => {
                let (dir, local, remote) = udp_sides(p)?;
                datagrams.push(Datagram {
                    t,
                    dir,
                    local: local.port(),
                    remote: remote.to_string(),
                    payload: hex(p.payload),
                });
            }
        }
    }

    for (c, [c2s, s2c]) in connections.iter().zip(&buffers) {
        ensure!(
            c2s.is_empty() && s2c.is_empty(),
            "{console} {} connection from port {} ends inside a frame ({} c2s / {} s2c bytes left)",
            c.role,
            c.client_port,
            c2s.len(),
            s2c.len()
        );
    }
    Ok(ConsoleRecording {
        console: console.to_owned(),
        connections,
        datagrams,
    })
}

/// Dolphin records the console's own UDP address as 0.0.0.0.
fn udp_sides(p: &Packet) -> Result<(UdpDir, SocketAddrV4, SocketAddrV4)> {
    match (p.src.ip().is_unspecified(), p.dst.ip().is_unspecified()) {
        (true, false) => Ok((UdpDir::Out, p.src, p.dst)),
        (false, true) => Ok((UdpDir::In, p.dst, p.src)),
        _ => bail!(
            "cannot tell the console's side of UDP {} -> {}",
            p.src,
            p.dst
        ),
    }
}

/// Reads every `raw/<console>.pcap` of a scenario and splits them on a shared clock.
pub fn load(scenario: &Path) -> Result<Vec<ConsoleRecording>> {
    let mut raws = Vec::new();
    for entry in
        fs::read_dir(scenario.join("raw")).with_context(|| format!("{}", scenario.display()))?
    {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "pcap") {
            let console = path.file_stem().unwrap().to_string_lossy().into_owned();
            raws.push((console, fs::read(&path)?));
        }
    }
    raws.sort();
    let captures = raws
        .iter()
        .map(|(console, data)| {
            pcap::parse(data)
                .map(|c| (console, c))
                .with_context(|| console.clone())
        })
        .collect::<Result<Vec<_>>>()?;
    let t0 = captures
        .iter()
        .flat_map(|(_, c)| c.packets.iter().filter(|p| keep(p)).map(|p| p.ts_us))
        .min()
        .context("no packets")?;
    captures
        .iter()
        .map(|(console, c)| split(console, &c.packets, t0))
        .collect()
}

/// The generated files of a scenario (`tcp/`, `udp/`), by path relative to the scenario directory.
pub fn render(recordings: &[ConsoleRecording]) -> Result<BTreeMap<PathBuf, String>> {
    let mut files = BTreeMap::new();
    for rec in recordings {
        for (n, c) in rec.connections.iter().enumerate() {
            let name = format!("tcp/{}-{:02}-{}.jsonl", rec.console, n + 1, c.role);
            files.insert(PathBuf::from(name), jsonl(&c.frames)?);
        }
        if !rec.datagrams.is_empty() {
            let name = format!("udp/{}.jsonl", rec.console);
            files.insert(PathBuf::from(name), jsonl(&rec.datagrams)?);
        }
    }
    Ok(files)
}

/// Imports a scenario: stores filtered copies of the given captures in `raw/`, then regenerates
/// `tcp/` and `udp/` from everything in `raw/`. With no captures, only regenerates.
pub fn import(scenario: &Path, captures: &[(String, PathBuf)]) -> Result<Vec<ConsoleRecording>> {
    fs::create_dir_all(scenario.join("raw"))?;
    for (console, path) in captures {
        let data = fs::read(path).with_context(|| format!("{}", path.display()))?;
        let capture = pcap::parse(&data).with_context(|| format!("{}", path.display()))?;
        let filtered = pcap::write(capture.header, capture.packets.iter().filter(|p| keep(p)));
        fs::write(
            scenario.join("raw").join(format!("{console}.pcap")),
            filtered,
        )?;
    }
    let recordings = load(scenario)?;
    for dir in ["tcp", "udp"] {
        let _ = fs::remove_dir_all(scenario.join(dir));
    }
    for (path, content) in render(&recordings)? {
        let path = scenario.join(path);
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(path, content)?;
    }
    Ok(recordings)
}

fn jsonl<T: Serialize>(items: &[T]) -> Result<String> {
    let mut out = String::new();
    for item in items {
        out.push_str(&serde_json::to_string(item)?);
        out.push('\n');
    }
    Ok(out)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(s: &str) -> Result<Vec<u8>> {
    ensure!(s.len().is_multiple_of(2), "odd hex length");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).with_context(|| format!("bad hex at {i}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pcap::tests::build;

    #[test]
    fn splits_connections_and_frames_across_segments() {
        // ReqLineCheck (no payload) split over two segments, then AnsLineCheck back.
        let data = build(&[
            (
                1_000,
                Proto::Tcp,
                "192.168.1.2:5000",
                "192.168.1.1:8202",
                &[0, 0, 0, 7],
            ),
            (
                2_000,
                Proto::Tcp,
                "192.168.1.2:5000",
                "192.168.1.1:8202",
                &[0x60, 0x01, 0x02, 0x00],
            ),
            (
                3_000,
                Proto::Tcp,
                "192.168.1.1:8202",
                "192.168.1.2:5000",
                &[0, 1, 0, 7, 0x60, 0x01, 0x02, 0x00, 0xaa],
            ),
            (
                4_000,
                Proto::Tcp,
                "192.168.1.2:6000",
                "192.168.1.1:80",
                b"POST /ac HTTP/1.1",
            ),
            (5_000, Proto::Udp, "0.0.0.0:21769", "127.0.0.1:27901", b"NN"),
            (6_000, Proto::Udp, "127.0.0.1:27901", "0.0.0.0:21769", b"ok"),
        ]);
        let capture = pcap::parse(&data).unwrap();
        let rec = split("dev1", &capture.packets, 1_000).unwrap();

        assert_eq!(rec.connections.len(), 1, "port 80 is dropped");
        let c = &rec.connections[0];
        assert_eq!((c.role, c.client_port), ("fmp", 5000));
        assert_eq!(c.frames.len(), 2);
        assert_eq!(
            (c.frames[0].t, c.frames[0].dir, c.frames[0].seq),
            (1_000, Dir::C2s, 7)
        );
        assert_eq!(c.frames[0].name, "AnsLineCheck");
        assert_eq!(
            (c.frames[1].dir, c.frames[1].payload.as_str()),
            (Dir::S2c, "aa")
        );

        assert_eq!(rec.datagrams.len(), 2);
        assert_eq!(
            (rec.datagrams[0].dir, rec.datagrams[0].local),
            (UdpDir::Out, 21769)
        );
        assert_eq!(rec.datagrams[1].remote, "127.0.0.1:27901");
    }

    #[test]
    fn rejects_a_stream_ending_inside_a_frame() {
        let data = build(&[(
            1,
            Proto::Tcp,
            "10.0.0.2:5000",
            "10.0.0.1:8200",
            &[0, 9, 0, 0, 0x60],
        )]);
        let capture = pcap::parse(&data).unwrap();
        assert!(split("dev1", &capture.packets, 0).is_err());
    }
}
