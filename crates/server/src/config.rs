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
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
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
    /// The public port (`public.rs`): the guide, health and metrics. Every interface by default.
    pub public: SocketAddr,
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
    /// The removed keys the file still sets: each is ignored, with a warning at startup. They stay accepted so an old
    /// `mh3sp.toml` keeps starting the server (no breaking change since the image is public): `bot_port` (the quest
    /// bots, tag `archive/quest-bots`), `api_port` and `api_address` (the API).
    pub ignored: Vec<&'static str>,
    /// Give consoles an encryption key at login (`../docs/history/packet encryption.md`).
    pub encryption: bool,
    /// Capcom IDs allowed the admins' chat commands (`game::commands`). ponytail: until ops are in the database
    /// (`access_rules`, kind `op`: mh3-rs/server#6).
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
    /// The name of the commands' socket (`control.rs`; no key, no environment variable: the commands find it by its
    /// name alone): the tests run several servers side by side, each on its own.
    pub control: String,
    /// Maintenance mode (`mh3sp maintenance "<message>"`, not an environment variable): OPN only, every console
    /// refused with this message, no database.
    pub maintenance: Option<String>,
    /// DNS servers standing in for the root servers when a domain `external_address` is looked up (`address.rs`; no
    /// key, no environment variable, `None` in production: the real root servers). Tests run one of their own.
    pub test_roots: Option<Vec<IpAddr>>,
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
    // The old names of these two (the port was for health and metrics only, on loopback): still honoured.
    #[serde(alias = "health_port")]
    public_port: Option<u16>,
    #[serde(alias = "health_address")]
    public_address: Option<IpAddr>,
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

    pub fn parse(text: &str) -> Result<Self> {
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
            public: SocketAddr::new(
                f.public_address.unwrap_or(IpAddr::from([0, 0, 0, 0])),
                listen("public_port", f.public_port, 8280)?.port(),
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
            ignored: [
                ("bot_port", f.bot_port.is_some()),
                ("api_port", f.api_port.is_some()),
                ("api_address", f.api_address.is_some()),
            ]
            .into_iter()
            .filter_map(|(key, set)| set.then_some(key))
            .collect(),
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
            control: crate::control::NAME.to_owned(),
            maintenance: None,
            test_roots: None,
        })
    }
}

/// A name the game can hold: letters, digits, `-` and `.` (no `%`: some of its hostnames are format strings).
pub fn domain(value: &str) -> Result<String> {
    let value = value.to_ascii_lowercase();
    let max = crate::patch::DOMAIN_MAX;
    if value.len() > max {
        bail!(
            "domain: {value:?} has {} characters, the patch fits {max}",
            value.len()
        );
    }
    if !labels(&value) {
        bail!("domain: {value:?} is not a domain name");
    }
    Ok(value)
}

/// Every label of `value` (split at `.`) is letters, digits and inner `-`s, not empty, and at most 63 bytes (DNS's own
/// limit: a longer one can't be asked for).
fn labels(value: &str) -> bool {
    let label = |l: &str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    };
    value.split('.').all(label)
}

/// What `external_address` holds, told apart when the server starts. The key itself stays a string, which consoles are
/// handed as it is: only the relay needs the number.
#[derive(Debug, PartialEq, Eq)]
pub enum External {
    /// Used as it is, never looked up.
    Ipv4(Ipv4Addr),
    /// Lowercase; resolved at its authoritative nameservers (`address.rs`). Unlike `domain`, it has no 25-character
    /// limit: that one is the patch's, which this key doesn't feed.
    Domain(String),
    /// Neither (an IPv6 address, a typo): not an error, so that an operator's file keeps starting the server
    /// (ADR 0016); the relay is off.
    Other,
}

impl External {
    pub fn parse(value: &str) -> Self {
        if let Ok(ip) = value.parse() {
            return Self::Ipv4(ip);
        }
        let name = value.to_ascii_lowercase();
        // A last label of digits only is a malformed IPv4 address ("300.1.1.1"), never a host name (RFC 3696, 2).
        let numeric = name
            .rsplit('.')
            .next()
            .is_some_and(|l| l.bytes().all(|b| b.is_ascii_digit()));
        match name.len() <= 253 && labels(&name) && !numeric {
            true => Self::Domain(name),
            false => Self::Other,
        }
    }
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

    /// Keys that left the server and that it still accepts and ignores (no breaking change since the image is
    /// public): an operator's old `mh3sp.toml` keeps starting it. They are no longer in the example.
    const REMOVED_KEYS: [&str; 3] = ["bot_port", "api_port", "api_address"];

    /// Keys that were renamed and still work (serde lists them with the others): the example documents the new names.
    const OLD_NAMES: [&str; 2] = ["health_port", "health_address"];

    /// A removed key still loads; a key that never existed is still refused, so a typo is never silently ignored.
    #[test]
    fn a_removed_key_still_loads() {
        // What each was set to in an operator's file.
        let line = |key: &str| match key {
            "api_address" => format!("{key} = \"127.0.0.1\""),
            _ => format!("{key} = 8290"),
        };
        for key in REMOVED_KEYS {
            assert!(toml::from_str::<File>(&line(key)).is_ok(), "{key}");
        }
        // Each one set is named, so that the server can warn about it; none set, nothing to say.
        let all = REMOVED_KEYS.map(line).join("\n");
        assert_eq!(Config::parse(&all).unwrap().ignored, REMOVED_KEYS);
        assert!(Config::parse("").unwrap().ignored.is_empty());
        assert!(Config::parse("bot_prot = 8290").is_err());
    }

    /// `deploy/mh3sp.toml.example` is the operators' reference: it names every key, and with each one uncommented
    /// the server takes it. The keys are serde's own, listed in its refusal of an unknown one; the removed keys
    /// ([`REMOVED_KEYS`]) are left out.
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
        let in_the_example =
            |k: &&str| *k != "secret" && !REMOVED_KEYS.contains(k) && !OLD_NAMES.contains(k);
        for key in keys.into_iter().filter(in_the_example) {
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
    fn the_external_address_is_an_ipv4_or_a_domain() {
        use External::{Domain, Ipv4, Other};
        let domain = |d: &str| Domain(d.to_owned());
        assert_eq!(
            External::parse("203.0.113.10"),
            Ipv4([203, 0, 113, 10].into())
        );
        assert_eq!(
            External::parse("Home.Example.ORG"),
            domain("home.example.org")
        );
        // Past the 25 characters the patch fits: this key doesn't feed the patch.
        let long = "a-very-long-dynamic-dns-name.example.org";
        assert!(long.len() > crate::patch::DOMAIN_MAX);
        assert_eq!(External::parse(long), domain(long));
        assert_eq!(External::parse("localhost"), domain("localhost"));
        for neither in [
            "2001:db8::1",
            "300.1.1.1",
            "203.0.113",
            "not a domain",
            "-bad.example.org",
            "example..org",
            "example.org.",
            "mh3.example.org:8201",
            "",
        ] {
            assert_eq!(External::parse(neither), Other, "{neither:?}");
        }
        assert_eq!(External::parse(&format!("{}.org", "a".repeat(250))), Other);
        // A label is 63 characters at most.
        let label = |n: usize| format!("{}.example.org", "a".repeat(n));
        assert_eq!(External::parse(&label(63)), domain(&label(63)));
        assert_eq!(External::parse(&label(64)), Other);
    }

    /// An operator's file keeps starting the server whatever `external_address` holds (ADR 0016).
    #[test]
    fn any_external_address_is_accepted_by_the_file() {
        for value in ["203.0.113.10", "home.example.org", "2001:db8::1", "a b"] {
            let c = Config::parse(&format!("external_address = {value:?}")).unwrap();
            assert_eq!(c.external_address.as_deref(), Some(value));
        }
    }

    #[test]
    fn an_empty_file_is_every_default() {
        let c = Config::parse("").unwrap();
        assert_eq!(c.opn.port(), 8200);
        assert_eq!(c.nas, SocketAddr::from(([0, 0, 0, 0], 80)));
        assert_eq!(
            c.public,
            SocketAddr::from(([0, 0, 0, 0], 8280)),
            "every interface"
        );
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

    /// The public port was the health port (loopback, for health and metrics only): its old keys still set it, so an
    /// operator's file keeps meaning what it said. Both names of the one key at once are an error, never a silent pick.
    #[test]
    fn the_public_port_keeps_its_old_keys() {
        let old = Config::parse("health_address = \"127.0.0.1\"\nhealth_port = 9280").unwrap();
        assert_eq!(old.public, SocketAddr::from(([127, 0, 0, 1], 9280)));
        let new = Config::parse("public_address = \"127.0.0.2\"\npublic_port = 9281").unwrap();
        assert_eq!(new.public, SocketAddr::from(([127, 0, 0, 2], 9281)));
        assert!(Config::parse("health_port = 1\npublic_port = 2").is_err());
        assert!(Config::parse("public_port = 0").is_err());
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
            public_address = "127.0.0.1"
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
        assert_eq!(c.public, SocketAddr::from(([127, 0, 0, 1], 8280)));
        assert_eq!(c.lmp.port(), 9201);
        assert!(c.encryption);
        assert_eq!(c.admins, ["ABC123"]);
        for wrong in [
            "external_adress = \"typo\"",
            "natneg_strategy = \"mesh\"",
            "relay_ports = \"2-1\"",
            "opn_port = 0",
            "opn_port = 70000",
            "public_address = \"localhost\"",
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
