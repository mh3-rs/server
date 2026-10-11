//! Server configuration, from a TOML file: `mh3sp.toml` in the working directory, or the file `MH3SP_CONFIG` names.
//! Every key is optional; a missing `mh3sp.toml` means every default (a missing `MH3SP_CONFIG` file is an error), and
//! an unknown key is an error (a typo would otherwise be a silent default). A secret's key also takes a `__file`
//! form, naming a file that holds it (Docker secrets): `secret__file`, never both. The server listens on `0.0.0.0`, and hands
//! consoles the LMP and FMP ports it listens on: a container must publish them unchanged (set the ports here, not in
//! the port mapping). The logger reads `RUST_LOG` (default `info,sqlx=warn`), not this file.
//!
//! Every key, its default and what it does: `deploy/mh3sp.toml.example`, the operators' reference (a test holds it
//! to `File`).
//!
//! Without `external_address` or `domain` the server runs but refuses every console ("not configured").
//! Without a secret it doesn't start (maintenance mode doesn't need one). The secret must never change once
//! consoles have registered: none of them would be recognised.

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::ops::RangeInclusive;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Clone, Debug)]
pub struct Config {
    /// `0.0.0.0` and the configured ports; tests bind port 0 to run servers side by side.
    pub opn: SocketAddr,
    pub lmp: SocketAddr,
    pub fmp: SocketAddr,
    pub health: SocketAddr,
    /// The API (`api.rs`).
    pub api: SocketAddr,
    /// The Nintendo login (`nas.rs`).
    pub nas: SocketAddr,
    /// UDP.
    pub qr: SocketAddr,
    pub natneg: SocketAddr,
    pub strategy: Strategy,
    pub relay_ports: RangeInclusive<u16>,
    pub tls_dir: PathBuf,
    pub database_url: String,
    pub external_address: Option<String>,
    /// Lowercase, checked: it goes into the game's memory (`patch.rs`).
    pub domain: Option<String>,
    pub secret: Option<Secret>,
    pub content_dir: PathBuf,
    /// Where the quest bots' brain connects (localhost only); none: no bots.
    pub bot_port: Option<u16>,
    /// Give consoles an encryption key at login (`../docs/history/packet encryption.md`).
    pub encryption: bool,
    /// Capcom IDs allowed the admins' chat commands (`game::commands`). ponytail: until the website's users and
    /// roles (`../docs/schema.md`, S2).
    pub admins: Vec<String>,
    /// The operator's texts (`text.rs`), each with a one-sentence default.
    pub name: String,
    pub announce: String,
    pub no_charge: String,
    pub terms: String,
    pub subterms: String,
    pub guildmaster: String,
    /// Seconds added to the system clock (not an environment variable, 0 in production): tests replay recordings
    /// at their date, since content depends on it.
    pub clock_offset: i64,
    /// Rate limits per console (`game::fmp`; not an environment variable, on in production): off in tests that send
    /// faster than any console (replays, the relay latency test).
    pub rate_limits: bool,
    /// Loopback (this machine: the lab, the tests) is exempt from the per-address limits (not an environment
    /// variable, on in production): off in the test of those limits' wiring.
    pub loopback_exempt: bool,
    /// Maintenance mode (`mh3sp maintenance "<message>"`, not an environment variable): OPN only, every console
    /// refused with this message, no database.
    pub maintenance: Option<String>,
}

/// A secret value: its `Debug` never shows it.
#[derive(Clone)]
pub struct Secret(pub Vec<u8>);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(…)")
    }
}

/// The file as written: every key optional.
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct File {
    opn_port: Option<u16>,
    lmp_port: Option<u16>,
    fmp_port: Option<u16>,
    health_port: Option<u16>,
    health_address: Option<IpAddr>,
    api_port: Option<u16>,
    api_address: Option<IpAddr>,
    nas_port: Option<u16>,
    qr_port: Option<u16>,
    natneg_port: Option<u16>,
    natneg_strategy: Option<Strategy>,
    relay_ports: Option<String>,
    tls_dir: Option<PathBuf>,
    database_url: Option<String>,
    external_address: Option<String>,
    domain: Option<String>,
    secret: Option<String>,
    #[serde(rename = "secret__file")]
    secret_file: Option<PathBuf>,
    content_dir: Option<PathBuf>,
    bot_port: Option<u16>,
    encryption: bool,
    admins: Vec<String>,
    name: Option<String>,
    announce: Option<String>,
    no_charge: Option<String>,
    terms: Option<String>,
    subterms: Option<String>,
    guildmaster: Option<String>,
}

impl Config {
    /// `MH3SP_CONFIG`, or `mh3sp.toml` if there is one.
    pub fn load() -> Result<Self> {
        let (path, required) = match std::env::var_os("MH3SP_CONFIG") {
            Some(path) => (PathBuf::from(path), true),
            None => (PathBuf::from("mh3sp.toml"), false),
        };
        let text = match std::fs::read_to_string(&path) {
            Err(e) if !required && e.kind() == std::io::ErrorKind::NotFound => String::new(),
            read => read.with_context(|| format!("read {}", path.display()))?,
        };
        Self::parse(&text).with_context(|| format!("{}", path.display()))
    }

    fn parse(text: &str) -> Result<Self> {
        // The message and its line, never the line itself: it may hold the secret.
        let f: File = toml::from_str(text).map_err(|e| {
            let line = e
                .span()
                .map_or(0, |s| text[..s.start].matches('\n').count() + 1);
            anyhow::anyhow!("line {line}: {}", e.message())
        })?;
        let empty = |v: Option<String>| v.filter(|v| !v.is_empty());
        Ok(Self {
            opn: listen("opn_port", f.opn_port, 8200)?,
            lmp: listen("lmp_port", f.lmp_port, 8201)?,
            fmp: listen("fmp_port", f.fmp_port, 8202)?,
            health: SocketAddr::new(
                f.health_address.unwrap_or(IpAddr::from([127, 0, 0, 1])),
                listen("health_port", f.health_port, 8280)?.port(),
            ),
            api: SocketAddr::new(
                f.api_address.unwrap_or(IpAddr::from([127, 0, 0, 1])),
                listen("api_port", f.api_port, 8281)?.port(),
            ),
            nas: listen("nas_port", f.nas_port, 80)?,
            qr: listen("qr_port", f.qr_port, 27900)?,
            natneg: listen("natneg_port", f.natneg_port, 27901)?,
            strategy: f.natneg_strategy.unwrap_or(Strategy::Hybrid),
            relay_ports: ports(f.relay_ports.as_deref().unwrap_or("40000-40099"))?,
            tls_dir: f.tls_dir.unwrap_or_else(|| "data/tls".into()),
            database_url: f
                .database_url
                .unwrap_or_else(|| "sqlite://data/mh3sp.db".into()),
            external_address: empty(f.external_address),
            domain: empty(f.domain).map(|d| domain(&d)).transpose()?,
            secret: secret(empty(f.secret), f.secret_file)?,
            content_dir: f.content_dir.unwrap_or_else(|| "content".into()),
            bot_port: f.bot_port,
            encryption: f.encryption,
            admins: f.admins,
            name: f.name.unwrap_or_else(|| NAME.into()),
            announce: f.announce.unwrap_or_else(|| ANNOUNCE.into()),
            no_charge: f.no_charge.unwrap_or_else(|| NO_CHARGE.into()),
            terms: f.terms.unwrap_or_else(|| TERMS.into()),
            subterms: f.subterms.unwrap_or_else(|| SUBTERMS.into()),
            guildmaster: f.guildmaster.unwrap_or_else(|| GUILDMASTER.into()),
            clock_offset: 0,
            rate_limits: true,
            loopback_exempt: true,
            maintenance: None,
        })
    }
}

/// A name the game can hold: letters, digits, `-` and `.` (no `%`: some of its hostnames are format strings).
fn domain(value: &str) -> Result<String> {
    let value = value.to_ascii_lowercase();
    let max = crate::patch::DOMAIN_MAX;
    if value.len() > max {
        bail!(
            "domain: {value:?} has {} characters, the patch fits {max}",
            value.len()
        );
    }
    let label = |l: &str| {
        !l.is_empty()
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    };
    if !value.split('.').all(label) {
        bail!("domain: {value:?} is not a domain name");
    }
    Ok(value)
}

/// The texts' defaults: a sentence each, the operator's to replace.
pub const NAME: &str = "Monster Hunter Tri";
pub const ANNOUNCE: &str = "<BODY><CENTER>Welcome, hunter!<END>";
pub const NO_CHARGE: &str = "<BODY><CENTER>Playing here is free.<END>";
pub const TERMS: &str = "Play fair and be kind to other hunters.";
pub const SUBTERMS: &str = "Have fun!";
pub const GUILDMASTER: &str = "Happy hunting!";

fn listen(key: &str, port: Option<u16>, default: u16) -> Result<SocketAddr> {
    match port.unwrap_or(default) {
        0 => bail!("{key}: 0 is not a port (1–65535)"),
        port => Ok(SocketAddr::from(([0, 0, 0, 0], port))),
    }
}

/// How consoles reach each other (M4 D2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Strategy {
    /// Directly, through the relay for the consoles that can't.
    Hybrid,
    P2p,
    Relay,
}

/// `first-last`.
fn ports(value: &str) -> Result<RangeInclusive<u16>> {
    let parsed = value
        .split_once('-')
        .and_then(|(a, b)| Some((a.trim().parse::<u16>().ok()?, b.trim().parse::<u16>().ok()?)))
        .filter(|&(a, b)| a > 0 && a <= b);
    let (first, last) =
        parsed.with_context(|| format!("relay_ports: {value:?} is not first-last"))?;
    Ok(first..=last)
}

/// `secret`, or `secret__file`, never both (an error, like an unknown key: which one counts would be a guess); the
/// file's trailing newline isn't part of it.
fn secret(value: Option<String>, file: Option<PathBuf>) -> Result<Option<Secret>> {
    let Some(path) = file else {
        return Ok(value.map(|v| Secret(v.into_bytes())));
    };
    if value.is_some() {
        bail!("secret and secret__file both set: keep one");
    }
    let mut bytes =
        std::fs::read(&path).with_context(|| format!("secret__file: read {}", path.display()))?;
    while bytes.last().is_some_and(|b| matches!(b, b'\n' | b'\r')) {
        bytes.pop();
    }
    if bytes.is_empty() {
        bail!("secret__file: {} is empty", path.display());
    }
    Ok(Some(Secret(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_file_without_its_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        std::fs::write(&path, "from-file\n").unwrap();
        let file = || Some(path.clone());
        assert_eq!(secret(None, file()).unwrap().unwrap().0, b"from-file");
        assert!(secret(Some("inline".into()), file()).is_err(), "both");
        assert_eq!(
            secret(Some("inline".into()), None).unwrap().unwrap().0,
            b"inline"
        );
        assert!(secret(None, None).unwrap().is_none());
        std::fs::write(&path, "\n").unwrap();
        assert!(secret(None, file()).is_err());
        assert!(secret(None, Some("/nonexistent".into())).is_err());
    }

    /// `deploy/mh3sp.toml.example` is the operators' reference: it names every key, and with each one uncommented
    /// the server takes it. The keys are serde's own, listed in its refusal of an unknown one.
    #[test]
    fn the_example_holds_every_key() {
        let example = include_str!("../../../deploy/mh3sp.toml.example");
        let refusal = toml::from_str::<File>("unknown = 1").err().unwrap();
        let keys: Vec<&str> = refusal.message().split('`').skip(3).step_by(2).collect();
        assert!(keys.len() > 20, "{}", refusal.message());
        let key_of = |line: &str| {
            let (key, _) = line.trim_start_matches("# ").split_once(" = ")?;
            key.bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'_')
                .then_some(key.to_string())
        };
        let named: Vec<String> = example.lines().filter_map(key_of).collect();
        // `secret` is named in prose: the example gives its file form, and both at once are refused.
        for key in keys.into_iter().filter(|&k| k != "secret") {
            assert!(named.iter().any(|n| n == key), "{key} not in the example");
        }

        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("secret");
        std::fs::write(&secret, "s").unwrap();
        let uncommented: String = example
            .lines()
            .map(|l| match key_of(l) {
                Some(_) => l.trim_start_matches("# "),
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n")
            .replace("\"/run/secrets/mh3sp\"", &format!("{secret:?}"));
        let c = Config::parse(&uncommented).unwrap();
        assert_eq!(c.domain.as_deref(), Some("mh3.example.org"));
    }

    #[test]
    fn an_empty_file_is_every_default() {
        let c = Config::parse("").unwrap();
        assert_eq!(c.opn.port(), 8200);
        assert_eq!(c.nas, SocketAddr::from(([0, 0, 0, 0], 80)));
        assert_eq!(c.health, SocketAddr::from(([127, 0, 0, 1], 8280)));
        assert_eq!(c.strategy, Strategy::Hybrid);
        assert_eq!(c.relay_ports, 40000..=40099);
        assert!(
            c.secret.is_none()
                && c.external_address.is_none()
                && c.domain.is_none()
                && !c.encryption
        );
        assert_eq!(c.tls_dir, PathBuf::from("data/tls"));
        assert_eq!(
            [
                c.name,
                c.announce,
                c.no_charge,
                c.terms,
                c.subterms,
                c.guildmaster
            ],
            [NAME, ANNOUNCE, NO_CHARGE, TERMS, SUBTERMS, GUILDMASTER]
        );
    }

    #[test]
    fn a_file_sets_and_checks_its_keys() {
        let c = Config::parse(
            r#"
            external_address = "203.0.113.10"
            domain = "MH3.Example.org"
            secret = "s"
            natneg_strategy = "relay"
            relay_ports = "41000-41010"
            health_address = "0.0.0.0"
            lmp_port = 9201
            encryption = true
            admins = ["ABC123"]
            announce = "<BODY><CENTER>Hello<END>"
            guildmaster = "Line one\nline two"
            "#,
        )
        .unwrap();
        assert_eq!(c.announce, "<BODY><CENTER>Hello<END>");
        assert_eq!(c.guildmaster, "Line one\nline two");
        assert_eq!(c.terms, TERMS);
        assert_eq!(c.external_address.as_deref(), Some("203.0.113.10"));
        assert_eq!(c.domain.as_deref(), Some("mh3.example.org"), "lowercase");
        assert_eq!(c.secret.unwrap().0, b"s");
        assert_eq!(c.strategy, Strategy::Relay);
        assert_eq!(c.relay_ports, 41000..=41010);
        assert_eq!(c.health, SocketAddr::from(([0, 0, 0, 0], 8280)));
        assert_eq!(c.lmp.port(), 9201);
        assert!(c.encryption);
        assert_eq!(c.admins, ["ABC123"]);
        for wrong in [
            "external_adress = \"typo\"",
            "natneg_strategy = \"mesh\"",
            "relay_ports = \"2-1\"",
            "opn_port = 0",
            "opn_port = 70000",
            "health_address = \"localhost\"",
            "encryption = \"on\"",
            "domain = \"abcdefghijklmn.example.org\"",
            "domain = \"%s.example.org\"",
            "domain = \"example..org\"",
            "domain = \"-a.example.org\"",
            "tls_cert = \"gone\"",
            "natneg_hostname = \"gone\"",
        ] {
            assert!(Config::parse(wrong).is_err(), "{wrong} accepted");
        }
        let longest = Config::parse("domain = \"abcdefghijklm.example.org\"").unwrap();
        assert_eq!(longest.domain.unwrap().len(), crate::patch::DOMAIN_MAX);
        let error = Config::parse("encryption = false\nsecret = hunter2").unwrap_err();
        assert!(!format!("{error:#}").contains("hunter2"), "{error:#}");
        assert!(format!("{error:#}").starts_with("line 2:"), "{error:#}");
        let error = Config::parse("encryption = false\ntypo = 1").unwrap_err();
        assert!(format!("{error:#}").starts_with("line 2:"), "{error:#}");
    }
}
