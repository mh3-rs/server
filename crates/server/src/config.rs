//! Server configuration. Ports and binding are fixed (`0.0.0.0`, OPN 8200, LMP 8201, FMP 8202, health 8280):
//! consoles are handed the LMP and FMP ports, so a container must publish them unchanged. The rest comes from
//! environment variables; a `.env` file is loaded first when present (dev).
//!
//! | Variable | Default |
//! |---|---|
//! | `TLS_CERT`, `TLS_KEY` | `certs/dev/server.crt`, `certs/dev/server.key` (signed by the CA the patched game trusts) |
//! | `DATABASE_URL` | `sqlite://data/mh3sp.db` (the name sqlx uses) |
//! | `RUST_LOG` | `info,sqlx=warn` (read by the logger, not here) |
//! | `EXTERNAL_ADDRESS` | none: the address consoles reach this server at, handed out for LMP and FMP (never guessed) |
//! | `NATNEG_HOSTNAME` | none: the only NAT negotiation hostname accepted in a disc's memory (our patch's) |
//! | `SECRET`, or `SECRET__FILE` naming a file that holds it | none: the pepper of the support codes' HMAC |
//!
//! Without `EXTERNAL_ADDRESS` or `NATNEG_HOSTNAME` the server runs but refuses every console ("not configured").
//! Without a secret it doesn't start (maintenance mode doesn't need one). The secret must never change once
//! consoles have registered: none of them would be recognised.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use tracing::warn;

/// The ports consoles are handed (`AnsLmpConnect`, `AnsFmpInfo`).
pub const LMP_PORT: u16 = 8201;
pub const FMP_PORT: u16 = 8202;

#[derive(Clone, Debug)]
pub struct Config {
    /// Fixed in production (see the module doc); tests set their own to run servers side by side.
    pub opn: SocketAddr,
    pub lmp: SocketAddr,
    pub fmp: SocketAddr,
    pub health: SocketAddr,
    pub tls_cert: PathBuf,
    pub tls_key: PathBuf,
    pub database_url: String,
    pub external_address: Option<String>,
    pub natneg_hostname: Option<String>,
    pub secret: Option<Secret>,
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

impl Config {
    pub const HEALTH: SocketAddr =
        SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 8280);

    pub fn from_env() -> Result<Self> {
        let get =
            |key: &str, default: &str| std::env::var(key).unwrap_or_else(|_| default.to_owned());
        let optional = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
        Ok(Self {
            opn: SocketAddr::from(([0, 0, 0, 0], 8200)),
            lmp: SocketAddr::from(([0, 0, 0, 0], LMP_PORT)),
            fmp: SocketAddr::from(([0, 0, 0, 0], FMP_PORT)),
            health: Self::HEALTH,
            tls_cert: get("TLS_CERT", "certs/dev/server.crt").into(),
            tls_key: get("TLS_KEY", "certs/dev/server.key").into(),
            database_url: get("DATABASE_URL", "sqlite://data/mh3sp.db"),
            external_address: optional("EXTERNAL_ADDRESS"),
            natneg_hostname: optional("NATNEG_HOSTNAME"),
            secret: secret(optional("SECRET"), optional("SECRET__FILE"))?,
            maintenance: None,
        })
    }
}

/// `SECRET__FILE` wins over `SECRET` (Docker secrets are files); the file's trailing newline isn't part of it.
fn secret(value: Option<String>, file: Option<String>) -> Result<Option<Secret>> {
    let Some(path) = file else {
        return Ok(value.map(|v| Secret(v.into_bytes())));
    };
    if value.is_some() {
        warn!("SECRET and SECRET__FILE both set: SECRET is ignored");
    }
    let mut bytes = std::fs::read(&path).with_context(|| format!("SECRET__FILE: read {path}"))?;
    while bytes.last().is_some_and(|b| matches!(b, b'\n' | b'\r')) {
        bytes.pop();
    }
    if bytes.is_empty() {
        bail!("SECRET__FILE: {path} is empty");
    }
    Ok(Some(Secret(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_wins_without_its_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        std::fs::write(&path, "from-file\n").unwrap();
        let file = Some(path.display().to_string());
        assert_eq!(
            secret(Some("from-env".into()), file).unwrap().unwrap().0,
            b"from-file"
        );
        assert_eq!(
            secret(Some("from-env".into()), None).unwrap().unwrap().0,
            b"from-env"
        );
        assert!(secret(None, None).unwrap().is_none());
        std::fs::write(&path, "\n").unwrap();
        assert!(secret(None, Some(path.display().to_string())).is_err());
        assert!(secret(None, Some("/nonexistent".into())).is_err());
    }
}
