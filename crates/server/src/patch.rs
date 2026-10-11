//! The players' patch (`../docs/history/patch and CA.md`): the deployment's CA in place of Nintendo's, and every
//! hostname the online mode uses under the deployment's `domain`. One table of addresses per region, three renderings:
//! Riivolution (Dolphin, a Wii booting the disc), `wit dolpatch` (an image for USB loaders) and Gecko codes (USB
//! loaders with Ocarina). Computed on demand, never stored: `mh3sp patch` writes it, the NAS port serves it (`nas.rs`).

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use bytes::BufMut;

use crate::tls;

/// The longest domain the patch fits: NatNeg's names (`a.<domain>`) have 28 bytes, NUL included, and the NAS's
/// host (the domain itself) 25 characters between `http://` and `/ac`.
pub const DOMAIN_MAX: usize = 25;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Pal,
    Us,
}

/// Where each patched object lives in a region's `main.dol` (virtual addresses).
struct Table {
    ca: u32,
    opn: u32,
    opn_original: &'static str,
    nas_ac: u32,
    nas_pr: u32,
    available: u32,
    natneg: [u32; 3],
}

const PAL: Table = Table {
    ca: 0x8057_2840,
    opn: 0x8060_1200,
    opn_original: "mmh-t1-opn04.mmh-service.capcom.co.jp",
    nas_ac: 0x8063_0920,
    nas_pr: 0x8063_09a0,
    available: 0x8063_1118,
    natneg: [0x8063_11a8, 0x8063_11c4, 0x8063_11e0],
};

const US: Table = Table {
    ca: 0x8057_1ea0,
    opn: 0x8060_0978,
    opn_original: "mmh-t1-opn03.mmh-service.capcom.co.jp",
    nas_ac: 0x8063_0060,
    nas_pr: 0x8063_00e0,
    available: 0x8063_0858,
    natneg: [0x8063_08e8, 0x8063_0904, 0x8063_0920],
};

/// Each string's room, NUL included: what the patch fills (the rest with NULs).
const OPN_ROOM: usize = 40;
const NAS_ROOM: usize = 36;
const AVAILABLE_ROOM: usize = 40;
const NATNEG_ROOM: usize = 28;
/// NatNeg's three servers, `mh3uswii.<label>.<domain>` once the game adds its name (`DWCi_NatNeg.c:761`).
const NATNEG_LABELS: [&str; 3] = ["a", "b", "c"];

impl Region {
    pub const ALL: [Region; 2] = [Region::Pal, Region::Us];

    pub fn from_game_id(id: &[u8]) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.game_id().as_bytes() == id)
    }

    pub fn game_id(self) -> &'static str {
        match self {
            Region::Pal => "RMHP08",
            Region::Us => "RMHE08",
        }
    }

    fn table(self) -> &'static Table {
        match self {
            Region::Pal => &PAL,
            Region::Us => &US,
        }
    }

    /// Where NatNeg's names start: OPN's memory check reads the first two there.
    pub fn natneg_names_at(self) -> u32 {
        self.table().natneg[0]
    }
}

/// The first NatNeg name the patch writes: a disc showing another was patched for another server, or not at all.
pub(crate) fn natneg_name(domain: &str) -> String {
    format!("{}.{domain}", NATNEG_LABELS[0])
}

/// The DNS records an operator creates, all pointing to the server.
fn records(domain: &str) -> Vec<String> {
    let mut names = vec![
        domain.to_owned(),
        format!("opn.{domain}"),
        format!("mh3uswii.available.{domain}"),
    ];
    names.extend(NATNEG_LABELS.map(|l| format!("mh3uswii.{l}.{domain}")));
    names
}

/// One write into the game's memory.
struct Edit {
    address: u32,
    value: Vec<u8>,
    /// What must be there: the patch skips a disc that doesn't match. None for the CA: its original is Nintendo's.
    original: Option<&'static str>,
    what: &'static str,
}

fn edits(region: Region, domain: &str, ca: &[u8]) -> Result<Vec<Edit>> {
    ensure!(
        ca.len() == tls::CA_SIZE,
        "the CA is {} bytes, the game's slot {}",
        ca.len(),
        tls::CA_SIZE
    );
    let t = region.table();
    let string = |address, room: usize, text: String, original, what| -> Result<Edit> {
        ensure!(
            text.len() < room,
            "{what}: {text:?} is longer than the game's {} bytes",
            room - 1
        );
        let mut value = Vec::with_capacity(room);
        value.put_slice(text.as_bytes());
        value.put_bytes(0, room - text.len());
        Ok(Edit {
            address,
            value,
            original: Some(original),
            what,
        })
    };
    let mut edits = vec![
        Edit {
            address: t.ca,
            value: ca.to_vec(),
            original: None,
            what: "OPN's CA",
        },
        string(
            t.opn,
            OPN_ROOM,
            format!("opn.{domain}"),
            t.opn_original,
            "OPN",
        )?,
        string(
            t.nas_ac,
            NAS_ROOM,
            format!("http://{domain}/ac"),
            "https://naswii.nintendowifi.net/ac",
            "Nintendo login",
        )?,
        string(
            t.nas_pr,
            NAS_ROOM,
            format!("http://{domain}/pr"),
            "https://naswii.nintendowifi.net/pr",
            "Nintendo login, profanity",
        )?,
        string(
            t.available,
            AVAILABLE_ROOM,
            format!("%s.available.{domain}"),
            "%s.available.gs.nintendowifi.net",
            "availability check",
        )?,
    ];
    let originals = [
        "natneg1.gs.nintendowifi.net",
        "natneg2.gs.nintendowifi.net",
        "natneg3.gs.nintendowifi.net",
    ];
    for ((address, label), original) in t.natneg.into_iter().zip(NATNEG_LABELS).zip(originals) {
        edits.push(string(
            address,
            NATNEG_ROOM,
            format!("{label}.{domain}"),
            original,
            "NAT negotiation",
        )?);
    }
    Ok(edits)
}

/// Riivolution's XML: one file per region, applied only to its disc (`<id>`).
fn riivolution(region: Region, domain: &str, ca: &[u8]) -> Result<String> {
    let (game, developer) = region.game_id().split_at(4);
    let mut xml = format!(
        "<wiidisc version=\"1\">\n\
         \t<id game=\"{game}\" developer=\"{developer}\" />\n\
         \t<options>\n\
         \t\t<section name=\"mh3sp\">\n\
         \t\t\t<option name=\"{domain}\" default=\"1\">\n\
         \t\t\t\t<choice name=\"Enabled\">\n\
         \t\t\t\t\t<patch id=\"{domain}\" />\n\
         \t\t\t\t</choice>\n\
         \t\t\t</option>\n\
         \t\t</section>\n\
         \t</options>\n\
         \t<patch id=\"{domain}\">\n"
    );
    for edit in edits(region, domain, ca)? {
        writeln!(xml, "\t\t<!-- {} -->", edit.what)?;
        writeln!(xml, "\t\t{}", memory(&edit))?;
    }
    xml.push_str("\t</patch>\n</wiidisc>\n");
    Ok(xml)
}

/// `wit dolpatch <main.dol> xml=<file>`: its `<memory>` lines alone.
fn wit(region: Region, domain: &str, ca: &[u8]) -> Result<String> {
    let mut xml = String::new();
    for edit in edits(region, domain, ca)? {
        writeln!(xml, "<!-- {} -->", edit.what)?;
        writeln!(xml, "{}", memory(&edit))?;
    }
    Ok(xml)
}

fn memory(edit: &Edit) -> String {
    let mut line = format!(
        "<memory offset=\"0x{:08X}\" value=\"{}\"",
        edit.address,
        hex::encode_upper(&edit.value)
    );
    if let Some(original) = edit.original {
        let _ = write!(line, " original=\"{}\"", hex::encode_upper(original));
    }
    line + " />"
}

/// Gecko codes for USB loaders with Ocarina (`codes/<game id>.gct`): one `06` code per edit, which writes its bytes
/// at the address. Untested on a Wii: whether a loader's hook suits MH3 (`recordings.md`).
fn gecko(region: Region, domain: &str, ca: &[u8]) -> Result<Vec<u8>> {
    let mut gct = Vec::new();
    gct.put_u64(0x00D0_C0DE_00D0_C0DE);
    for edit in edits(region, domain, ca)? {
        // `06`: 25 bits of address (`ba` = 0x80000000), the length, the bytes padded to 8.
        gct.put_u32(0x0600_0000 | (edit.address & 0x01FF_FFFF));
        gct.put_u32(u32::try_from(edit.value.len())?);
        gct.put_slice(&edit.value);
        gct.put_bytes(0, edit.value.len().next_multiple_of(8) - edit.value.len());
    }
    gct.put_u64(0xF000_0000_0000_0000);
    Ok(gct)
}

/// Every file of the patch, by its path: what `mh3sp patch` writes and `/patch/<path>` serves. XMLs are named after
/// the domain (a player may keep several servers' patches); a `.gct` can't be (loaders look for `<game id>.gct`).
pub(crate) fn files(domain: &str, ca: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut files = Vec::new();
    for region in Region::ALL {
        let id = region.game_id();
        files.push((
            format!("riivolution/{domain}-{id}.xml"),
            riivolution(region, domain, ca)?.into_bytes(),
        ));
        files.push((
            format!("wit/{domain}-{id}.xml"),
            wit(region, domain, ca)?.into_bytes(),
        ));
        files.push((format!("codes/{id}.gct"), gecko(region, domain, ca)?));
    }
    Ok(files)
}

/// `mh3sp patch <dir>`: every file. The CA is created first if `tls_dir` is empty, as at startup. Returns the DNS
/// records the operator needs.
pub fn write(out: &Path, domain: &str, tls_dir: &Path) -> Result<Vec<String>> {
    tls::prepare(tls_dir)?;
    for (path, bytes) in files(domain, &tls::ca_der(tls_dir)?)? {
        let path = out.join(path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        std::fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(records(domain))
}

/// The page players download the patch from (`GET /patch` on the NAS port, the domain's port 80).
pub(crate) fn page(domain: &str) -> String {
    let link = |path: String, label: &str| format!("<a href=\"/patch/{path}\">{label}</a>");
    let both = |path: &dyn Fn(&str) -> String| {
        format!(
            "{} · {}",
            link(path(Region::Pal.game_id()), "European disc (RMHP08)"),
            link(path(Region::Us.game_id()), "US disc (RMHE08)")
        )
    };
    format!(
        "<!doctype html>\n<meta charset=\"utf-8\">\n<title>Monster Hunter Tri on {domain}</title>\n\
         <h1>Monster Hunter Tri on {domain}</h1>\n\
         <p>The patch for this server: it points the game here and makes it trust this server's certificate. Take the \
         file for your disc and the way you play.</p>\n<ul>\n\
         <li><b>Dolphin, or a Wii booting the disc</b> (Riivolution): {}. Into the <code>riivolution</code> folder \
         of the SD card (Dolphin: <code>Load/Riivolution/riivolution</code> in its user folder).</li>\n\
         <li><b>A USB loader</b>, patching your image once on a computer with <code>wit dolpatch</code>: {}.</li>\n\
         <li><b>A USB loader with Ocarina</b> (untested): {}. Into <code>codes</code> on the SD card, Ocarina on.</li>\n\
         </ul>\n",
        both(&|id| format!("riivolution/{domain}-{id}.xml")),
        both(&|id| format!("wit/{domain}-{id}.xml")),
        both(&|id| format!("codes/{id}.gct")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const LONGEST: &str = "abcdefghijklm.example.org";

    fn ca() -> Vec<u8> {
        vec![0x30; tls::CA_SIZE]
    }

    #[test]
    fn the_longest_domain_fits_every_string() {
        assert_eq!(LONGEST.len(), DOMAIN_MAX);
        for region in Region::ALL {
            let edits = edits(region, LONGEST, &ca()).unwrap();
            assert_eq!(edits.len(), 8);
            let natneg = edits
                .iter()
                .find(|e| e.address == region.natneg_names_at())
                .unwrap();
            assert!(
                natneg
                    .value
                    .starts_with(format!("{}\0", natneg_name(LONGEST)).as_bytes())
            );
        }
        assert!(
            edits(Region::Pal, &format!("x{LONGEST}"), &ca()).is_err(),
            "26 characters"
        );
    }

    #[test]
    fn each_string_fills_its_room_and_names_its_original() {
        for edit in edits(Region::Pal, "mh3.example.org", &ca()).unwrap() {
            match edit.original {
                None => assert_eq!(edit.value, ca(), "only the CA has no original"),
                Some(original) => {
                    assert!(
                        original.len() < edit.value.len(),
                        "{}: the original and its NUL fit",
                        edit.what
                    );
                    assert_eq!(*edit.value.last().unwrap(), 0, "{}", edit.what);
                }
            }
        }
        assert_eq!(
            &edits(Region::Pal, "mh3.example.org", &ca()).unwrap()[2].value[..27],
            b"http://mh3.example.org/ac\0\0"
        );
    }

    #[test]
    fn a_ca_of_another_size_is_refused() {
        assert!(edits(Region::Pal, "mh3.example.org", &[0; 925]).is_err());
    }

    #[test]
    fn riivolution_names_its_disc_and_carries_every_edit() {
        let xml = riivolution(Region::Us, "mh3.example.org", &ca()).unwrap();
        assert!(xml.contains("<id game=\"RMHE\" developer=\"08\" />"));
        assert_eq!(xml.matches("<memory ").count(), 8);
        assert!(xml.contains(&format!("value=\"{}\"", hex::encode_upper(ca()))));
        assert!(xml.contains(&hex::encode_upper("opn.mh3.example.org")));
        assert_eq!(
            xml.matches("<patch id=\"mh3.example.org\"").count(),
            2,
            "referenced and defined"
        );
        let wit = wit(Region::Us, "mh3.example.org", &ca()).unwrap();
        assert_eq!(wit.matches("<memory ").count(), 8);
        assert!(wit.contains("offset=\"0x80571EA0\""));
    }

    #[test]
    fn gecko_writes_each_edit_with_one_code_within_a_loaders_room() {
        for region in Region::ALL {
            check_gecko(region);
        }
    }

    fn check_gecko(region: Region) {
        let gct = gecko(region, LONGEST, &ca()).unwrap();
        assert!(gct.starts_with(&[0x00, 0xD0, 0xC0, 0xDE, 0x00, 0xD0, 0xC0, 0xDE]));
        assert!(gct.ends_with(&[0xF0, 0, 0, 0, 0, 0, 0, 0]));
        // USB Loader GX with its debugger on (`patchcode.c`).
        assert!(gct.len() <= 1864, "{} bytes", gct.len());
        let mut codes = &gct[8..gct.len() - 8];
        for edit in edits(region, LONGEST, &ca()).unwrap() {
            let word = |at: usize| u32::from_be_bytes(codes[at..at + 4].try_into().unwrap());
            assert_eq!(
                word(0),
                0x0600_0000 | (edit.address & 0x01FF_FFFF),
                "{}",
                edit.what
            );
            assert_eq!(word(4) as usize, edit.value.len());
            assert_eq!(&codes[8..8 + edit.value.len()], edit.value);
            codes = &codes[8 + edit.value.len().next_multiple_of(8)..];
        }
        assert!(codes.is_empty());
    }

    #[test]
    fn the_page_links_every_file() {
        let page = page("mh3.example.org");
        for (path, _) in files("mh3.example.org", &ca()).unwrap() {
            assert!(page.contains(&format!("href=\"/patch/{path}\"")), "{path}");
        }
    }

    #[test]
    fn the_records_cover_every_name_the_game_looks_up() {
        assert_eq!(
            records("mh3.example.org"),
            [
                "mh3.example.org",
                "opn.mh3.example.org",
                "mh3uswii.available.mh3.example.org",
                "mh3uswii.a.mh3.example.org",
                "mh3uswii.b.mh3.example.org",
                "mh3uswii.c.mh3.example.org",
            ]
        );
    }
}
