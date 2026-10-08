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
