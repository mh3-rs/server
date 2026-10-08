//! Corpus tooling.
//!
//! `corpus import <scenario-dir> [<console>=<capture.pcap> ...]`
//!   Stores filtered copies of the captures in `raw/` and regenerates `tcp/` and `udp/` from `raw/`.
//!   Without captures, only regenerates.
//!
//! `corpus replay <scenario-dir>... [--host <ip>] [--before <command>]`
//!   Replays the scenarios against a server (default 127.0.0.1, ports 8200-8202) and reports
//!   every server frame that differs from the recording. Exits 1 if any does. `--before` runs a
//!   command before each scenario, to give it a fresh server (e.g. `tools/oracle.sh`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use testkit::replay::{self, ConnectionReport, Got, Targets};

const USAGE: &str = "usage: corpus import <scenario-dir> [<console>=<capture.pcap> ...]
       corpus replay <scenario-dir>... [--host <ip>] [--before <command>]";

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("import") if args.len() >= 2 => import(&args[1], &args[2..]),
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
    let mut scenarios = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--host" => host = it.next().context("--host needs an ip")?.parse()?,
            "--before" => before = Some(it.next().context("--before needs a command")?.clone()),
            _ => scenarios.push(PathBuf::from(arg)),
        }
    }
    let targets = Targets::host(host);
    let mut clean = true;
    for scenario in &scenarios {
        if let Some(cmd) = &before {
            let status = std::process::Command::new("sh")
                .args(["-c", cmd])
                .status()?;
            anyhow::ensure!(status.success(), "`{cmd}` failed");
        }
        let reports = replay::replay(scenario, &targets).await?;
        clean &= print_report(scenario, &reports);
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
