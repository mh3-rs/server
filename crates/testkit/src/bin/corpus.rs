//! Corpus tooling.
//!
//! `corpus import <scenario-dir> [<console>=<capture.pcap> ...]`
//!   Stores filtered copies of the captures in `raw/` and regenerates `tcp/` and `udp/` from `raw/`.
//!   Without captures, only regenerates.
//!
//! `corpus diff <scenario-a> <scenario-b>`
//!   Compares two recordings of the same scenario and prints, per packet, the byte ranges that
//!   differ: the candidates for `corpus/masks.toml`.
//!
//! `corpus replay <scenario-dir>... [--host <ip>] [--before <command>] [--nudge] [--retries <n>]`
//!   Replays the scenarios against a server (default 127.0.0.1, ports 8200-8202) and reports
//!   every server frame that differs from the recording. Exits 1 if any does. `--before` runs a
//!   command before each scenario, to give it a fresh server (e.g. `tools/oracle.sh`). `--nudge`
//!   works around Python MH3SP's stalled reads (see `Targets::nudge`); use it only against Python.
//!   `--retries <n>` re-runs a failing scenario up to n more times (each after `--before`): Python
//!   MH3SP interleaves frames written to one socket from two threads, so multi-console scenarios
//!   are flaky against it. Never use it against our server.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use testkit::masks::Masks;
use testkit::replay::{self, ConnectionReport, Got, Targets};

const USAGE: &str = "usage: corpus import <scenario-dir> [<console>=<capture.pcap> ...]
       corpus diff <scenario-a> <scenario-b>
       corpus replay <scenario-dir>... [--host <ip>] [--before <command>] [--nudge] [--retries <n>]";

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("import") if args.len() >= 2 => import(&args[1], &args[2..]),
        Some("diff") if args.len() == 3 => diff(&args[1], &args[2]),
        Some("replay") if args.len() >= 2 => {
            if !replay_all(&args[1..]).await? {
                std::process::exit(1);
            }
            Ok(())
        }
        _ => bail!(USAGE),
    }
}

fn import(scenario: &str, captures: &[String]) -> Result<()> {
    let captures = captures
        .iter()
        .map(|a| {
            let (console, path) = a
                .split_once('=')
                .context("expected <console>=<capture.pcap>")?;
            Ok((console.to_owned(), PathBuf::from(path)))
        })
        .collect::<Result<Vec<_>>>()?;
    for rec in testkit::corpus::import(scenario.as_ref(), &captures)? {
        let frames: usize = rec.connections.iter().map(|c| c.frames.len()).sum();
        let roles: Vec<_> = rec.connections.iter().map(|c| c.role).collect();
        println!(
            "{}: {} connections ({}), {} frames, {} datagrams",
            rec.console,
            rec.connections.len(),
            roles.join(" "),
            frames,
            rec.datagrams.len()
        );
    }
    Ok(())
}

/// Returns whether every scenario replayed cleanly.
async fn replay_all(args: &[String]) -> Result<bool> {
    let mut host = "127.0.0.1".parse()?;
    let mut before = None;
    let mut nudge = false;
    let mut retries = 0;
    let mut scenarios = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--host" => host = it.next().context("--host needs an ip")?.parse()?,
            "--nudge" => nudge = true,
            "--retries" => retries = it.next().context("--retries needs a number")?.parse()?,
            "--before" => before = Some(it.next().context("--before needs a command")?.clone()),
            _ => scenarios.push(PathBuf::from(arg)),
        }
    }
    let targets = Targets {
        nudge,
        ..Targets::host(host)
    };
    // Masks live next to the scenarios: corpus/masks.toml.
    let masks_path = scenarios
        .first()
        .and_then(|s| s.parent())
        .unwrap_or(Path::new("."))
        .join("masks.toml");
    let masks = if masks_path.exists() {
        Masks::load(&masks_path)?
    } else {
        Masks::default()
    };
    let mut clean = true;
    for scenario in &scenarios {
        for attempt in 0..=retries {
            if let Some(cmd) = &before {
                let status = std::process::Command::new("sh")
                    .args(["-c", cmd])
                    .status()?;
                anyhow::ensure!(status.success(), "`{cmd}` failed");
            }
            let reports = replay::replay(scenario, &targets, &masks).await?;
            let last = attempt == retries;
            if reports.iter().all(ConnectionReport::is_clean) || last {
                clean &= print_report(scenario, &reports);
                if attempt > 0 {
                    println!("  (attempt {} of {})", attempt + 1, retries + 1);
                }
                break;
            }
        }
    }
    Ok(clean)
}

fn print_report(scenario: &Path, reports: &[ConnectionReport]) -> bool {
    let name = scenario.file_name().unwrap_or_default().to_string_lossy();
    let (matched, expected): (usize, usize) = reports
        .iter()
        .fold((0, 0), |(m, e), r| (m + r.matched, e + r.expected));
    let clean = reports.iter().all(ConnectionReport::is_clean);
    println!(
        "{} {name}: {matched}/{expected} server frames match",
        if clean { "✓" } else { "✗" }
    );
    for r in reports.iter().filter(|r| !r.extras.is_empty()) {
        let names: Vec<_> = r.extras.iter().map(|f| f.id.to_string()).collect();
        println!(
            "  {}: tolerated extra notifications: {}",
            r.name,
            names.join(", ")
        );
    }
    for r in reports.iter().filter(|r| !r.is_clean()) {
        println!("  {}: {}/{} match", r.name, r.matched, r.expected);
        for m in r.mismatches.iter().take(5) {
            let e = &m.expected;
            let detail = match &m.got {
                Got::Closed => "connection closed".to_owned(),
                Got::Timeout => "nothing received".to_owned(),
                Got::Frame(f) if format!("{:08x}", f.id.0) != e.id => format!("got {}", f.id),
                Got::Frame(f) => payload_diff(&e.payload, &f.payload),
            };
            println!("    #{} {}({}): {detail}", m.index, e.name, e.id);
        }
        if r.mismatches.len() > 5 {
            println!("    … {} more", r.mismatches.len() - 5);
        }
        if let Some(reason) = &r.aborted {
            println!("    stopped: {reason}");
        }
    }
    clean
}

/// `payload differs at byte 2 (13 bytes): expected …4142… got …5859…`
fn payload_diff(expected_hex: &str, got: &[u8]) -> String {
    let expected = testkit::corpus::unhex(expected_hex).unwrap_or_default();
    if expected == got {
        return "same payload, different seq".to_owned();
    }
    let at = expected
        .iter()
        .zip(got)
        .position(|(a, b)| a != b)
        .unwrap_or(expected.len().min(got.len()));
    let window = |b: &[u8]| testkit::corpus::hex(&b[at.min(b.len())..(at + 12).min(b.len())]);
    let lens = if expected.len() == got.len() {
        format!("{} bytes", got.len())
    } else {
        format!("{} vs {} bytes", expected.len(), got.len())
    };
    format!(
        "payload differs at byte {at} ({lens}): expected {} got {}",
        window(&expected),
        window(got)
    )
}

fn diff(a: &str, b: &str) -> Result<()> {
    let ascii = |b: &[u8]| -> String {
        b.iter()
            .map(|&c| if c.is_ascii_graphic() { c as char } else { '.' })
            .collect()
    };
    for d in testkit::diff::diff(a.as_ref(), b.as_ref())? {
        if d.ranges.is_empty() {
            println!(
                "{} #{} {:?} packet sequence differs: {}",
                d.connection, d.index, d.dir, d.name
            );
            continue;
        }
        let lens = if d.a.len() == d.b.len() {
            format!("{} bytes", d.a.len())
        } else {
            format!("{} vs {} bytes", d.a.len(), d.b.len())
        };
        println!(
            "{} #{} {:?} {} ({lens})",
            d.connection, d.index, d.dir, d.name
        );
        for &(start, end) in &d.ranges {
            let (x, y) = (
                &d.a[start.min(d.a.len())..end.min(d.a.len())],
                &d.b[start.min(d.b.len())..end.min(d.b.len())],
            );
            let (hx, hy) = (testkit::corpus::hex(x), testkit::corpus::hex(y));
            println!(
                "    [{start}..{end}) {hx} \"{}\" | {hy} \"{}\"",
                ascii(x),
                ascii(y)
            );
        }
    }
    Ok(())
}
