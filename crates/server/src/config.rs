//! Server configuration: one TOML file, with `DATABASE_URL` overriding the database URL
//! (handy for containers). See `config.example.toml`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: Listen,
    pub tls: Tls,
    pub database: Url,
}

/// Where each role listens. A role without an address is disabled.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Listen {
    pub opn: Option<SocketAddr>,
    pub lmp: Option<SocketAddr>,
    pub fmp: Option<SocketAddr>,
    /// HTTP health endpoints: `/health/live`, `/health/ready`.
    pub health: SocketAddr,
}

/// OPN's certificate: it must be signed by the CA the patched game trusts.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tls {
    pub cert: PathBuf,
    pub key: PathBuf,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Url {
    pub url: String,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("{}", path.display()))?;
        let mut config: Config =
            toml::from_str(&text).with_context(|| format!("{}", path.display()))?;
        if let Ok(url) = std::env::var("DATABASE_URL") {
            config.database.url = url;
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.example.toml");
        let config = Config::load(&path).unwrap();
        assert!(config.listen.opn.is_some());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err = toml::from_str::<Config>("typo = 1").unwrap_err();
        assert!(err.to_string().contains("typo"), "{err}");
    }
}
