//! Compares two recordings of the same scenario frame by frame, to find the volatile bytes.

use std::path::Path;

use anyhow::Result;

use crate::corpus::{self, Dir, FrameRecord};

/// A frame that differs between the two recordings.
#[derive(Debug)]
pub struct Difference {
    /// e.g. `dev1-02-lmp`.
    pub connection: String,
    pub index: usize,
    pub dir: Dir,
    pub name: String,
    pub a: Vec<u8>,
    pub b: Vec<u8>,
    /// Differing byte ranges `[start, end)`, merged when closer than 4 bytes.
    pub ranges: Vec<(usize, usize)>,
}

/// Differences between two recordings. Connections are matched by name, frames by position;
/// a connection whose packet sequence differs is reported once, as a structural difference
/// (empty `ranges`), and not compared further.
pub fn diff(a: &Path, b: &Path) -> Result<Vec<Difference>> {
    let (a, b) = (corpus::load(a)?, corpus::load(b)?);
    let mut out = Vec::new();
    for (ra, rb) in a.iter().zip(&b) {
        for (n, (ca, cb)) in ra.connections.iter().zip(&rb.connections).enumerate() {
            let connection = format!("{}-{:02}-{}", ra.console, n + 1, ca.role);
            let (fa, fb) = (significant(&ca.frames), significant(&cb.frames));
            for (index, (x, y)) in fa.iter().zip(&fb).enumerate() {
                let (pa, pb) = (hex::decode(&x.payload)?, hex::decode(&y.payload)?);
                if x.id != y.id {
                    out.push(Difference {
                        connection: connection.clone(),
                        index,
                        dir: x.dir,
                        name: format!("{} vs {}", x.name, y.name),
                        a: pa,
                        b: pb,
                        ranges: Vec::new(),
                    });
                    break;
                }
                if pa != pb {
                    let ranges = ranges(&pa, &pb);
                    out.push(Difference {
                        connection: connection.clone(),
                        index,
                        dir: x.dir,
                        name: x.name.clone(),
                        a: pa,
                        b: pb,
                        ranges,
                    });
                }
            }
        }
    }
    Ok(out)
}

/// Frames that are not time-driven line checks.
fn significant(frames: &[FrameRecord]) -> Vec<&FrameRecord> {
    frames
        .iter()
        .filter(|f| !f.name.ends_with("LineCheck"))
        .collect()
}

fn ranges(a: &[u8], b: &[u8]) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for i in 0..a.len().max(b.len()) {
        if a.get(i) == b.get(i) {
            continue;
        }
        match out.last_mut() {
            Some((_, end)) if i <= *end + 3 => *end = i + 1,
            _ => out.push((i, i + 1)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_nearby_differences() {
        assert_eq!(ranges(b"abcdefghij", b"abXdeYghij"), vec![(2, 6)]);
        assert_eq!(ranges(b"abcdefghij", b"Xbcdefghij"), vec![(0, 1)]);
        assert_eq!(ranges(b"abc", b"abcde"), vec![(3, 5)]);
    }
}
