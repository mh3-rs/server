//! Content the consoles download (`ReqBinaryHead`/`Data`/`Foot`), built when asked (decision D2 of M2): a binary's
//! version is derived from its bytes, so a change reaches connected consoles at their next `ReqBinaryVersion`.
//!
//! Types `0x01`–`0x0f` are the English set (the console asks for those, D7). Event content (quests, rotation, trading
//! post) is MH3SP's files, in their format, under `CONTENT_DIR` (`event/quest_rotation.json`, the quest files it
//! lists, `event/trading_post.json`); never in this repository.

use std::path::Path;
use std::time::SystemTime;

use anyhow::{Context as _, Result, bail};
use serde_json::Value;
use tracing::error;

use crate::text::{Language, Text, text};

const DAY: u64 = 24 * 60 * 60;
/// The Jhen Mohran cycle, in days: fog the first day, Jhen Mohran (sandstorm) the next two.
const JHEN_CYCLE: u64 = 14;
const JHEN_START: u64 = 1;
const JHEN_END: u64 = 3;
/// Each text of the NPC greeters binary (`0x03`) takes this many bytes.
const GREETER_SIZE: usize = 0x180;

/// HR range per server type (Open, Rookie, Expert, Recruiting).
const SERVER_TYPE_HR: [(u16, u16); 4] = [(0, 999), (0, 30), (31, 999), (0, 999)];
/// City HR limits (None, Low, High, then unused ones).
const CITY_HR_LIMITS: [(u16, u16); 8] = [
    (0, 999),
    (0, 30),
    (31, 999),
    (0, 999),
    (0, 999),
    (0, 999),
    (0, 999),
    (0, 999),
];
/// Ticks of a day/night cycle (6.4 hours; the game defaults to 3000 when 0).
pub const TICKS_PER_CYCLE: u32 = 23040;

/// A content binary: its bytes and the version they give.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binary {
    pub version: u32,
    pub bytes: Vec<u8>,
}

impl Binary {
    fn new(bytes: Vec<u8>) -> Self {
        let hash = openssl::sha::sha256(&bytes);
        Binary {
            version: u32::from_be_bytes([hash[0], hash[1], hash[2], hash[3]]),
            bytes,
        }
    }
}

/// What content depends on.
pub struct Context<'a> {
    /// `CONTENT_DIR`.
    pub dir: &'a Path,
    pub started: SystemTime,
    pub now: SystemTime,
    pub language: Language,
}

impl Context<'_> {
    /// Days since the epoch: content rotates daily.
    fn day(&self) -> u64 {
        secs(self.now) / DAY
    }
}

/// The binary of type `kind`, or `None` for a type the server doesn't know; an error when its content files are
/// missing or broken.
pub fn binary(kind: u8, ctx: &Context) -> Result<Option<Binary>> {
    let bytes = match kind {
        0x01 => server_types(ctx.started, ctx.language),
        0x02 => jhen_times(ctx.day()),
        0x03 => greeters(ctx)?,
        0x04 => trading_post(ctx)?,
        // Unknown purpose (the game fetches it as file 5): MH3SP's placeholder, which the console accepts.
        0x05 => b"5".repeat(0x10),
        _ => return Ok(None),
    };
    Ok(Some(Binary::new(bytes)))
}

/// Reads every content file once and logs what is wrong (not fatal: a download needing a broken file fails).
pub fn check(dir: &Path) {
    let result = (|| -> Result<()> {
        for quest in rotation(dir)?.iter().flatten() {
            quest_info(dir, quest)?;
        }
        trading_post_days(dir)?;
        Ok(())
    })();
    if let Err(e) = result {
        error!(error = %format!("{e:#}"), dir = %dir.display(), "content");
    }
}

fn secs(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn read_json(path: &Path) -> Result<Value> {
    let text = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&text).with_context(|| format!("parse {}", path.display()))
}

/// The quest files of each rotation day (paths relative to `CONTENT_DIR`, as MH3SP writes them).
fn rotation(dir: &Path) -> Result<Vec<Vec<String>>> {
    let path = dir.join("event/quest_rotation.json");
    serde_json::from_value(read_json(&path)?)
        .with_context(|| format!("{}: a list of days, each a list of files", path.display()))
}

/// A quest's name, and whether it's an arena quest.
fn quest_info(dir: &Path, file: &str) -> Result<(String, bool)> {
    let path = dir.join(file);
    let quest = read_json(&path)?;
    let info = &quest["quest_info"];
    let Some(name) = info["name"].as_str() else {
        bail!("{}: no quest_info.name", path.display());
    };
    // The flags are four bytes written as bits, least significant first; arena quests have bit 4 of the last one.
    let arena = info["flags"][3][4]
        .as_u64()
        .with_context(|| format!("{}: no quest_info.flags", path.display()))?;
    Ok((name.to_owned(), arena == 1))
}

/// Each day of the trading post: (offered, price 1, price 2), each an (item, quantity) pair.
fn trading_post_days(dir: &Path) -> Result<Vec<Vec<[[u16; 2]; 3]>>> {
    let path = dir.join("event/trading_post.json");
    serde_json::from_value(read_json(&path)?)
        .with_context(|| format!("{}: days of [[item, qty] ×3] entries", path.display()))
}

/// Fog start, Jhen Mohran start, Jhen Mohran end (epoch seconds): the current cycle while it lasts, else the next.
fn jhen_times(day: u64) -> Vec<u8> {
    let in_cycle = day % JHEN_CYCLE;
    let start = if in_cycle < JHEN_END {
        day - in_cycle
    } else {
        day - in_cycle + JHEN_CYCLE
    };
    [start, start + JHEN_START, start + JHEN_END]
        .iter()
        .flat_map(|d| ((d * DAY) as u32).to_be_bytes())
        .collect()
}

fn jhen_active(day: u64) -> bool {
    (JHEN_START..JHEN_END).contains(&(day % JHEN_CYCLE))
}

/// What the town's NPCs say: shops, trading post, the event and arena quests of the day, the guildmaster.
fn greeters(ctx: &Context) -> Result<Vec<u8>> {
    let days = rotation(ctx.dir)?;
    if days.is_empty() {
        bail!(
            "{}: no rotation day",
            ctx.dir.join("event/quest_rotation.json").display()
        );
    }
    let mut events = Vec::new();
    let mut arena = Vec::new();
    for file in &days[(ctx.day() % days.len() as u64) as usize] {
        let (name, is_arena) = quest_info(ctx.dir, file)?;
        if is_arena {
            arena.push(name)
        } else {
            events.push(name)
        }
    }
    let sale = jhen_active(ctx.day());
    let mut event_text = String::new();
    if sale {
        event_text.push_str(&text(Text::JhenFestival, ctx.language));
        event_text.push('\n');
    }
    event_text.push_str(&events.join("\n"));
    let mut out = Vec::with_capacity(6 * GREETER_SIZE);
    for t in [
        text(Text::ToolShop { sale }, ctx.language).into_owned(),
        text(Text::MaterialShop { sale }, ctx.language).into_owned(),
        text(Text::TradingPostOpen, ctx.language).into_owned(),
        event_text,
        arena.join("\n"),
        text(Text::Guildmaster, ctx.language).into_owned(),
    ] {
        pad(&mut out, &t, GREETER_SIZE);
    }
    Ok(out)
}

/// The trading post of the day: per offer, three (item, quantity) pairs and an empty one.
fn trading_post(ctx: &Context) -> Result<Vec<u8>> {
    let days = trading_post_days(ctx.dir)?;
    if days.is_empty() {
        bail!(
            "{}: no trading post day",
            ctx.dir.join("event/trading_post.json").display()
        );
    }
    let mut out = Vec::new();
    for entry in &days[(ctx.day() % days.len() as u64) as usize] {
        for [item, qty] in entry.iter().chain([&[0, 0]]) {
            out.extend_from_slice(&item.to_be_bytes());
            out.extend_from_slice(&qty.to_be_bytes());
        }
    }
    Ok(out)
}

/// `s` in a field of `size` bytes, NUL-padded, the last byte always NUL.
fn pad(out: &mut Vec<u8>, s: &str, size: usize) {
    let start = out.len();
    out.extend_from_slice(&s.as_bytes()[..s.len().min(size - 1)]);
    out.resize(start + size, 0);
}

/// Server types (names, descriptions, HR ranges), city seekings, HR limits and goals, timeouts.
fn server_types(started: SystemTime, language: Language) -> Vec<u8> {
    let mut out = Vec::with_capacity(5068);
    for i in 0..SERVER_TYPE_HR.len() {
        pad(&mut out, &text(Text::ServerTypeName(i), language), 24);
        pad(
            &mut out,
            &text(Text::ServerTypeDescription(i), language),
            168,
        );
    }
    out.extend_from_slice(&0u32.to_be_bytes());
    // The server's start, as the reference of the day/night cycle.
    let started = started
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as u32);
    out.extend_from_slice(&started.to_be_bytes());
    out.extend_from_slice(&TICKS_PER_CYCLE.to_be_bytes());
    for i in 0..32 {
        pad(&mut out, &text(Text::CitySeeking(i), language), 48);
        // Unknown flags (MH3SP's values).
        out.extend_from_slice(b"\0\x01\xff\0");
    }
    for (i, (min, max)) in CITY_HR_LIMITS.iter().enumerate() {
        pad(&mut out, &text(Text::CityHrLimit(i), language), 30);
        // Enabled; restricted by HR.
        out.extend_from_slice(&[1, 0]);
        out.extend_from_slice(&min.to_be_bytes());
        out.extend_from_slice(&max.to_be_bytes());
    }
    for i in 0..64 {
        pad(&mut out, &text(Text::CityGoal(i), language), 30);
        // Enabled; no restriction; no HR range.
        out.extend_from_slice(&[1, 0, 0, 0, 0, 0]);
    }
    for (min, max) in SERVER_TYPE_HR {
        out.extend_from_slice(&min.to_be_bytes());
        out.extend_from_slice(&max.to_be_bytes());
    }
    // 8 timeouts, 30 s each, in frames (30 per second).
    for _ in 0..8 {
        out.extend_from_slice(&(30u16 * 30).to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_types_have_the_recorded_size() {
        let ctx = Context {
            dir: Path::new("/nonexistent"),
            started: SystemTime::now(),
            now: SystemTime::now(),
            language: Language::English,
        };
        let b = binary(0x01, &ctx).unwrap().unwrap();
        assert_eq!(b.bytes.len(), 5068);
        assert_eq!(&b.bytes[..5], b"Open\0");
        assert!(binary(0x7f, &ctx).unwrap().is_none());
        // Event content without its files: an error, not a panic.
        assert!(binary(0x04, &ctx).is_err());
    }

    #[test]
    fn jhen_mohran_cycle() {
        // Day 0 of a cycle: fog today, Jhen Mohran tomorrow until day 3.
        let t = jhen_times(14 * 1000);
        let day =
            |i: usize| u32::from_be_bytes(t[i * 4..i * 4 + 4].try_into().unwrap()) as u64 / DAY;
        assert_eq!((day(0), day(1), day(2)), (14000, 14001, 14003));
        // Day 5: the next cycle.
        assert_eq!(
            u32::from_be_bytes(jhen_times(14005)[..4].try_into().unwrap()) as u64 / DAY,
            14014
        );
        assert!(
            !jhen_active(14000) && jhen_active(14001) && jhen_active(14002) && !jhen_active(14003)
        );
    }
}
