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
    /// Absent: to the end of the payload, whose length may then differ (a divergence where the Rust server sends
    /// other records than the recording).
    pub len: Option<usize>,
    pub var: Option<String>,
    pub payload_len: Option<usize>,
    /// Hex bytes.
    pub pattern: Option<String>,
    /// Why the Rust server differs there on purpose: required in `divergences.toml`.
    pub reason: Option<String>,
}

#[derive(Debug, Default)]
pub struct Masks(Vec<Mask>, Vec<Omit>);

/// A server frame the Rust server doesn't send, on purpose (`[[omit]]` in `divergences.toml`): the recording's
/// frame of `packet` with `seq` on `connection` (`dev1-05-fmp`) of `scenario` isn't expected; with `alert`, an answer
/// the Rust server replaces with an alert (a refusal: the answer's id with kind byte 0x01, its text not compared).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Omit {
    pub scenario: String,
    pub connection: String,
    pub packet: String,
    pub seq: u16,
    #[serde(default)]
    pub alert: bool,
    pub reason: String,
}

impl From<Vec<Mask>> for Masks {
    fn from(masks: Vec<Mask>) -> Self {
        Masks(masks, Vec::new())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MaskFile {
    #[serde(default)]
    mask: Vec<Mask>,
    #[serde(default)]
    omit: Vec<Omit>,
}

fn read(path: &Path) -> Result<MaskFile> {
    let text = std::fs::read_to_string(path).with_context(|| format!("{}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("{}", path.display()))
}

impl Masks {
    /// The masks of a corpus folder: `masks.toml` and, when present, `divergences.toml`.
    pub fn load(corpus: &Path) -> Result<Self> {
        let mut masks = read(&corpus.join("masks.toml"))?.mask;
        let mut omits = Vec::new();
        let divergences = corpus.join("divergences.toml");
        if divergences.exists() {
            let more = read(&divergences)?;
            if let Some(m) = more.mask.iter().find(|m| m.reason.is_none()) {
                anyhow::bail!(
                    "{}: divergence on {} without a reason",
                    divergences.display(),
                    m.packet
                );
            }
            masks.extend(more.mask);
            omits = more.omit;
        }
        for m in &masks {
            if m.len.is_none() && (m.var.is_some() || m.pattern.is_some()) {
                anyhow::bail!(
                    "mask on {}: without a len, neither var nor pattern",
                    m.packet
                );
            }
            if let Some(pattern) = &m.pattern {
                hex::decode(pattern).with_context(|| format!("mask on {}: pattern", m.packet))?;
            }
        }
        Ok(Self(masks, omits))
    }

    /// What the Rust server does on purpose instead of sending this recorded frame: `Some(true)` an alert in its
    /// place, `Some(false)` nothing.
    pub fn omitted(
        &self,
        scenario: &str,
        connection: &str,
        packet: &str,
        seq: u16,
    ) -> Option<bool> {
        self.1
            .iter()
            .find(|o| {
                o.scenario == scenario
                    && o.connection == connection
                    && o.packet == packet
                    && o.seq == seq
            })
            .map(|o| o.alert)
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

    pub fn bind(&mut self, var: &str, recorded: &[u8], live: &[u8]) {
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
    let mut live = live;
    // Masks to the end: only what comes before them is compared.
    for m in masks.of(packet, recorded.len()).filter(|m| m.len.is_none()) {
        if m.offset <= expected.len() && m.offset <= live.len() {
            expected.truncate(m.offset);
            live = &live[..m.offset];
        }
    }
    if expected.len() != live.len() {
        return false;
    }
    let mut learned = Vec::new();
    let mut ranges = Vec::new();
    for m in masks.of(packet, recorded.len()).filter(|m| m.len.is_some()) {
        match &m.pattern {
            None => ranges.push((m, m.offset)),
            Some(pattern) => {
                let pattern = hex::decode(pattern).expect("checked at load");
                let at = expected.windows(pattern.len()).enumerate();
                ranges.extend(
                    at.filter(|(_, w)| *w == pattern)
                        .map(|(i, _)| (m, i + m.offset)),
                );
            }
        }
    }
    for (m, start) in ranges {
        let range = start..start + m.len.expect("filtered");
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
        Masks::from(vec![
            Mask {
                packet: "AnsTicket".into(),
                offset: 2,
                len: Some(3),
                var: Some("pat_ticket".into()),
                payload_len: None,
                pattern: None,
                reason: None,
            },
            Mask {
                packet: "AnsServerTime".into(),
                offset: 0,
                len: Some(2),
                var: None,
                payload_len: None,
                pattern: None,
                reason: None,
            },
            Mask {
                packet: "AnsFmpInfo".into(),
                offset: 2,
                len: Some(2),
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
    fn a_mask_to_the_end_compares_what_comes_before() {
        let masks = Masks::from(vec![Mask {
            packet: "AnsBlackList".into(),
            offset: 4,
            len: None,
            var: None,
            payload_len: None,
            pattern: None,
            reason: Some("test".into()),
        }]);
        let mut b = Bindings::default();
        let recorded = b"\0\0\0\0\0\0\0\x01blank record";
        assert!(matches(
            &masks,
            &mut b,
            "AnsBlackList",
            recorded,
            b"\0\0\0\0\0\0\0\0"
        ));
        assert!(!matches(
            &masks,
            &mut b,
            "AnsBlackList",
            recorded,
            b"\0\0\0\x01\0\0\0\0"
        ));
    }

    #[test]
    fn corpus_masks_parse() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
        assert!(!Masks::load(&path).unwrap().0.is_empty());
    }
}
