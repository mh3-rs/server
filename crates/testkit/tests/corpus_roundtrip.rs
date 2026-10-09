//! Tier 1 on real data: every recorded scenario must frame cleanly, re-encode byte for byte,
//! and match its committed `tcp/` and `udp/` files.
//!
//! Runs over `corpus/` at the repository root, or over `$CORPUS_DIR` if set.

use std::fs;
use std::path::PathBuf;

use bytes::{Bytes, BytesMut};
use testkit::corpus::{self, Dir};
use tokio_util::codec::Encoder;
use wire::{Frame, FrameCodec, PacketId};

fn scenarios() -> Vec<PathBuf> {
    let root = std::env::var_os("CORPUS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus"));
    let Ok(entries) = fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut dirs: Vec<_> = entries
        .map(|e| e.unwrap().path())
        .filter(|p| p.join("raw").is_dir())
        .collect();
    dirs.sort();
    dirs
}

#[test]
fn every_stream_reencodes_byte_for_byte() {
    for scenario in scenarios() {
        for rec in corpus::load(&scenario).unwrap() {
            for c in &rec.connections {
                for (d, dir) in [Dir::C2s, Dir::S2c].into_iter().enumerate() {
                    let mut out = BytesMut::new();
                    for f in c.frames.iter().filter(|f| f.dir == dir) {
                        let frame = Frame {
                            seq: f.seq,
                            id: PacketId(u32::from_str_radix(&f.id, 16).unwrap()),
                            payload: Bytes::from(corpus::unhex(&f.payload).unwrap()),
                        };
                        FrameCodec.encode(frame, &mut out).unwrap();
                    }
                    assert!(
                        out[..] == c.streams[d][..],
                        "{} {} {} port {} {dir:?}: re-encoded stream differs",
                        scenario.display(),
                        rec.console,
                        c.role,
                        c.client_port
                    );
                }
            }
        }
    }
}

#[test]
fn committed_files_match_the_raw_captures() {
    for scenario in scenarios() {
        let expected = corpus::render(&corpus::load(&scenario).unwrap()).unwrap();
        for (path, content) in &expected {
            let actual = fs::read_to_string(scenario.join(path)).unwrap_or_default();
            assert!(
                actual == *content,
                "{}: stale or missing, rerun `corpus import {}`",
                scenario.join(path).display(),
                scenario.display()
            );
        }
        for dir in ["tcp", "udp"] {
            for entry in fs::read_dir(scenario.join(dir)).into_iter().flatten() {
                let rel = PathBuf::from(dir).join(entry.unwrap().file_name());
                assert!(
                    expected.contains_key(&rel),
                    "{}: not generated from raw/",
                    rel.display()
                );
            }
        }
    }
}

/// Real payloads known to start with a `PatData` record (per MH3SP's handlers): the record decodes,
/// re-encodes byte for byte, and for `AnsConnection` (the console's connection data) fills the payload.
#[test]
fn real_pat_data_records_reencode() {
    use wire::{PatWrite, Reader};
    let mut checked = 0;
    for scenario in scenarios() {
        for rec in corpus::load(&scenario).unwrap() {
            for f in rec.connections.iter().flat_map(|c| &c.frames) {
                if !matches!(f.name.as_str(), "AnsConnection" | "ReqCircleCreate") {
                    continue;
                }
                let payload = corpus::unhex(&f.payload).unwrap();
                let mut reader = Reader::new(&payload);
                let data = reader.pat_data().unwrap();
                if f.name == "AnsConnection" {
                    reader.finish().unwrap();
                }
                let mut out = Vec::new();
                out.put_pat_data(&data).unwrap();
                assert_eq!(
                    out,
                    payload[..out.len()],
                    "{}: {}",
                    scenario.display(),
                    f.name
                );
                checked += 1;
            }
        }
    }
    if !scenarios().is_empty() {
        assert!(checked > 0, "no PatData payload found in the corpus");
    }
}

/// Every recorded payload of a typed packet decodes whole and re-encodes byte for byte; every type is seen at
/// least once, except those never recorded.
#[test]
fn typed_packets_reencode() {
    use std::collections::BTreeSet;
    use wire::packets::{TYPED, reencode};

    let mut seen = BTreeSet::new();
    let mut failures = BTreeSet::<String>::new();
    for scenario in scenarios() {
        for rec in corpus::load(&scenario).unwrap() {
            for c in &rec.connections {
                for f in &c.frames {
                    let payload = corpus::unhex(&f.payload).unwrap();
                    let id = PacketId(u32::from_str_radix(&f.id, 16).unwrap());
                    let Some(result) = reencode(id, &payload) else {
                        continue;
                    };
                    let at = format!(
                        "{} {} {} seq {}",
                        scenario.display(),
                        rec.console,
                        c.role,
                        f.seq
                    );
                    match result {
                        Ok(out) if out == payload => {}
                        Ok(_) => {
                            failures.insert(format!("{}: re-encodes differently ({at})", f.name));
                        }
                        Err(e) => {
                            failures.insert(format!("{}: {e} ({at})", f.name));
                        }
                    }
                    seen.insert(f.name.clone());
                }
            }
        }
    }
    // One line per packet type is enough to see what to fix.
    let mut by_type = std::collections::BTreeMap::new();
    for f in &failures {
        by_type
            .entry(f.split(':').next().unwrap().to_owned())
            .or_insert(f);
    }
    assert!(
        by_type.is_empty(),
        "{} layouts wrong:\n{}",
        by_type.len(),
        by_type
            .values()
            .map(|f| f.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    if !scenarios().is_empty() {
        let never_recorded = ["ReqMaintenance", "AnsMaintenance", "NtcCircleBreak"];
        let missing: Vec<_> = TYPED
            .iter()
            .map(|(_, name)| *name)
            .filter(|n| !seen.contains(*n) && !never_recorded.contains(n))
            .collect();
        assert!(missing.is_empty(), "never seen in the corpus: {missing:?}");
    }
}
