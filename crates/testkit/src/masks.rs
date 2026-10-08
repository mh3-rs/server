//! Volatile bytes (`corpus/masks.toml`), declared divergences (`corpus/divergences.toml`), and the recorded → live
//! values learned from them.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Where a volatile value first appears in a server frame, or with `pattern`, wherever that pattern appears (the
/// offset then counts from each occurrence). See `corpus/masks.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mask {
    pub packet: String,
    pub offset: usize,
    pub len: usize,
    pub var: Option<String>,
    pub payload_len: Option<usize>,
    /// Hex bytes.
    pub pattern: Option<String>,
    /// Why the Rust server differs there on purpose: required in `divergences.toml`.
    pub reason: Option<String>,
}

#[derive(Debug, Default)]
pub struct Masks(Vec<Mask>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MaskFile {
    mask: Vec<Mask>,
}

fn read(path: &Path) -> Result<Vec<Mask>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("{}", path.display()))?;
    let file: MaskFile = toml::from_str(&text).with_context(|| format!("{}", path.display()))?;
    Ok(file.mask)
}

impl Masks {
    /// The masks of a corpus folder: `masks.toml` and, when present, `divergences.toml`.
    pub fn load(corpus: &Path) -> Result<Self> {
        let mut masks = read(&corpus.join("masks.toml"))?;
        let divergences = corpus.join("divergences.toml");
        if divergences.exists() {
            let more = read(&divergences)?;
            if let Some(m) = more.iter().find(|m| m.reason.is_none()) {
                anyhow::bail!(
                    "{}: divergence on {} without a reason",
                    divergences.display(),
                    m.packet
                );
            }
            masks.extend(more);
        }
        for m in &masks {
            if let Some(pattern) = &m.pattern {
                crate::corpus::unhex(pattern)
                    .with_context(|| format!("mask on {}: pattern", m.packet))?;
            }
        }
        Ok(Self(masks))
    }

    fn of<'a>(&'a self, packet: &'a str, len: usize) -> impl Iterator<Item = &'a Mask> + 'a {
        self.0
            .iter()
            .filter(move |m| m.packet == packet && m.payload_len.is_none_or(|l| l == len))
    }
}

/// Recorded values and the live values that replaced them during a replay.
#[derive(Debug, Default)]
pub struct Bindings(Vec<Binding>);

#[derive(Debug)]
pub struct Binding {
    pub var: String,
    pub recorded: Vec<u8>,
    pub live: Vec<u8>,
}

impl Bindings {
    /// Replaces every occurrence of a bound recorded value by its live value.
    pub fn apply(&self, payload: &mut [u8]) {
        for b in &self.0 {
            let n = b.recorded.len();
            let mut i = 0;
            while i + n <= payload.len() {
                if payload[i..i + n] == b.recorded[..] {
                    payload[i..i + n].copy_from_slice(&b.live);
                    i += n;
                } else {
                    i += 1;
                }
            }
        }
    }

    fn bind(&mut self, var: &str, recorded: &[u8], live: &[u8]) {
        if recorded != live && !self.0.iter().any(|b| b.recorded == recorded) {
            self.0.push(Binding {
                var: var.to_owned(),
                recorded: recorded.to_vec(),
                live: live.to_vec(),
            });
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Binding> {
        self.0.iter()
    }
}

/// Whether a live server payload matches the recorded one, once known values are substituted and
/// masked bytes ignored. On a match, learns the bindings of the masks with a `var`; a failed
/// comparison learns nothing, so trying several candidates is safe.
pub fn matches(
    masks: &Masks,
    bindings: &mut Bindings,
    packet: &str,
    recorded: &[u8],
    live: &[u8],
) -> bool {
    let mut expected = recorded.to_vec();
    bindings.apply(&mut expected);
    if expected.len() != live.len() {
        return false;
    }
    let mut learned = Vec::new();
    let mut ranges = Vec::new();
    for m in masks.of(packet, recorded.len()) {
        match &m.pattern {
            None => ranges.push((m, m.offset)),
            Some(pattern) => {
                let pattern = crate::corpus::unhex(pattern).expect("checked at load");
                let at = expected.windows(pattern.len()).enumerate();
                ranges.extend(
                    at.filter(|(_, w)| *w == pattern)
                        .map(|(i, _)| (m, i + m.offset)),
                );
            }
        }
    }
    for (m, start) in ranges {
        let range = start..start + m.len;
        if range.end > live.len() {
            continue;
        }
        if let Some(var) = &m.var {
            learned.push((var, range.clone()));
        }
        expected[range.clone()].copy_from_slice(&live[range]);
    }
    if expected != live {
        return false;
    }
    for (var, range) in learned {
        bindings.bind(var, &recorded[range.clone()], &live[range]);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn masks() -> Masks {
        Masks(vec![
            Mask {
                packet: "AnsTicket".into(),
                offset: 2,
                len: 3,
                var: Some("pat_ticket".into()),
                payload_len: None,
                pattern: None,
                reason: None,
            },
            Mask {
                packet: "AnsServerTime".into(),
                offset: 0,
                len: 2,
                var: None,
                payload_len: None,
                pattern: None,
                reason: None,
            },
            Mask {
                packet: "AnsFmpInfo".into(),
                offset: 2,
                len: 2,
                var: None,
                payload_len: None,
                pattern: Some("0302200a".into()),
                reason: None,
            },
        ])
    }

    #[test]
    fn a_pattern_masks_every_occurrence() {
        let (masks, mut bindings) = (masks(), Bindings::default());
        let recorded = b"\x03\x02\x20\x0a..\x03\x02\x20\x0a";
        assert!(matches(
            &masks,
            &mut bindings,
            "AnsFmpInfo",
            recorded,
            b"\x03\x02\x99\x99..\x03\x02\x98\x98"
        ));
        assert!(!matches(
            &masks,
            &mut bindings,
            "AnsFmpInfo",
            recorded,
            b"\x03\x02\x99\x99.!\x03\x02\x98\x98"
        ));
    }

    #[test]
    fn binds_then_substitutes_everywhere() {
        let (masks, mut bindings) = (masks(), Bindings::default());
        assert!(matches(
            &masks,
            &mut bindings,
            "AnsTicket",
            b"\0\x03ABC",
            b"\0\x03XYZ"
        ));

        // The console echoes the recorded ticket: it must be sent as the live one.
        let mut sent = b"..ABC..".to_vec();
        bindings.apply(&mut sent);
        assert_eq!(sent, b"..XYZ..");

        // A later server frame repeating the ticket matches without a mask of its own.
        assert!(matches(&masks, &mut bindings, "AnsOther", b"ABC!", b"XYZ!"));
        assert!(!matches(
            &masks,
            &mut bindings,
            "AnsOther",
            b"ABC!",
            b"XYZ?"
        ));
    }

    #[test]
    fn ignores_masked_bytes_only() {
        let (masks, mut bindings) = (masks(), Bindings::default());
        assert!(matches(
            &masks,
            &mut bindings,
            "AnsServerTime",
            b"\x01\x02\x03",
            b"\x09\x09\x03"
        ));
        assert!(!matches(
            &masks,
            &mut bindings,
            "AnsServerTime",
            b"\x01\x02\x03",
            b"\x09\x09\x04"
        ));
        assert!(!matches(
            &masks,
            &mut bindings,
            "AnsServerTime",
            b"\x01\x02",
            b"\x01\x02\x03"
        ));
    }

    #[test]
    fn corpus_masks_parse() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
        assert!(!Masks::load(&path).unwrap().0.is_empty());
    }
}
