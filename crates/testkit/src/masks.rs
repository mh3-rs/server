//! Volatile bytes (`corpus/masks.toml`) and the recorded → live values learned from them.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Where a volatile value first appears in a server frame. See `corpus/masks.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mask {
    pub packet: String,
    pub offset: usize,
    /// Absent: to the end of the payload, whose length may then differ (an address and a port, say).
    pub len: Option<usize>,
    pub var: Option<String>,
    pub payload_len: Option<usize>,
}

#[derive(Debug, Default)]
pub struct Masks(Vec<Mask>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MaskFile {
    mask: Vec<Mask>,
}

impl Masks {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("{}", path.display()))?;
        let file: MaskFile =
            toml::from_str(&text).with_context(|| format!("{}", path.display()))?;
        if let Some(m) = file
            .mask
            .iter()
            .find(|m| m.var.is_some() && m.len.is_none())
        {
            anyhow::bail!(
                "{}: mask on {}: a var needs a len",
                path.display(),
                m.packet
            );
        }
        Ok(Self(file.mask))
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
    let mut live = live;
    // Masks to the end: only what comes before is compared.
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
    for m in masks.of(packet, recorded.len()) {
        let Some(len) = m.len else { continue };
        let range = m.offset..m.offset + len;
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
                len: Some(3),
                var: Some("pat_ticket".into()),
                payload_len: None,
            },
            Mask {
                packet: "AnsServerTime".into(),
                offset: 0,
                len: Some(2),
                var: None,
                payload_len: None,
            },
        ])
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
    fn a_mask_to_the_end_ignores_length() {
        let masks = Masks(vec![Mask {
            packet: "AnsLmpConnect".into(),
            offset: 1,
            len: None,
            var: None,
            payload_len: None,
        }]);
        let mut bindings = Bindings::default();
        assert!(matches(
            &masks,
            &mut bindings,
            "AnsLmpConnect",
            b"\x00\x0f192.168",
            b"\x00\x09127"
        ));
        assert!(!matches(
            &masks,
            &mut bindings,
            "AnsLmpConnect",
            b"\x00\x0f192",
            b"\x01\x09127"
        ));
    }

    #[test]
    fn corpus_masks_parse() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/masks.toml");
        assert!(!Masks::load(&path).unwrap().0.is_empty());
    }
}
