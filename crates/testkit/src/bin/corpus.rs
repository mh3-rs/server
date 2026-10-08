//! `corpus import <scenario-dir> [<console>=<capture.pcap> ...]`
//!
//! Stores filtered copies of the captures in `<scenario-dir>/raw/` and regenerates `tcp/` and
//! `udp/` from `raw/`. Without captures, only regenerates.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some("import"), Some(scenario)) = (args.first().map(String::as_str), args.get(1)) else {
        bail!("usage: corpus import <scenario-dir> [<console>=<capture.pcap> ...]");
    };
    let captures = args[2..]
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
