//! Server configuration, from environment variables (a `.env` file is loaded first when present, for dev).
//! Every setting has a working default, so only what differs from it needs setting.
//!
//! | Variable | Default |
//! |---|---|
//! | `MH3SP_OPN_ADDR`, `MH3SP_LMP_ADDR`, `MH3SP_FMP_ADDR` | `0.0.0.0:8200`, `:8201`, `:8202`; empty = role disabled |
//! | `MH3SP_HEALTH_ADDR` | `0.0.0.0:8280` (`/health/live`, `/health/ready`) |
//! | `MH3SP_TLS_CERT`, `MH3SP_TLS_KEY` | `certs/dev/server.crt`, `certs/dev/server.key` (signed by the CA the patched game trusts) |
//! | `DATABASE_URL` | `sqlite://data/mh3sp.db` (the name sqlx uses) |
//! | `RUST_LOG` | `info,sqlx=warn` (read by the logger, not here) |

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub struct Config {
    /// Where each role listens; `None` = disabled.
    pub opn: Option<SocketAddr>,
    pub lmp: Option<SocketAddr>,
    pub fmp: Option<SocketAddr>,
    pub health: SocketAddr,
    pub tls_cert: PathBuf,
    pub tls_key: PathBuf,
    pub database_url: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::from(|key| std::env::var(key).ok())
    }

    /// Builds the config from any variable source (the environment, or a map in tests).
    pub fn from(var: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let get = |key: &str, default: &str| var(key).unwrap_or_else(|| default.to_owned());
        let addr = |key: &str, default: &str| -> Result<Option<SocketAddr>> {
            let value = get(key, default);
            if value.is_empty() {
                return Ok(None);
            }
            value
                .parse()
                .map(Some)
                .with_context(|| format!("{key}={value:?} is not an address"))
        };
        Ok(Self {
            opn: addr("MH3SP_OPN_ADDR", "0.0.0.0:8200")?,
            lmp: addr("MH3SP_LMP_ADDR", "0.0.0.0:8201")?,
            fmp: addr("MH3SP_FMP_ADDR", "0.0.0.0:8202")?,
            health: addr("MH3SP_HEALTH_ADDR", "0.0.0.0:8280")?
                .context("MH3SP_HEALTH_ADDR can't be empty")?,
            tls_cert: get("MH3SP_TLS_CERT", "certs/dev/server.crt").into(),
            tls_key: get("MH3SP_TLS_KEY", "certs/dev/server.key").into(),
            database_url: get("DATABASE_URL", "sqlite://data/mh3sp.db"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(vars: &[(&str, &str)]) -> Result<Config> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from(|key| vars.get(key).cloned())
    }

    #[test]
    fn defaults_cover_everything() {
        let c = config(&[]).unwrap();
        assert_eq!(c.opn, Some("0.0.0.0:8200".parse().unwrap()));
        assert_eq!(c.health.port(), 8280);
        assert_eq!(c.database_url, "sqlite://data/mh3sp.db");
    }

    #[test]
    fn empty_disables_a_role_and_bad_values_name_their_variable() {
        assert_eq!(config(&[("MH3SP_FMP_ADDR", "")]).unwrap().fmp, None);
        let err = config(&[("MH3SP_LMP_ADDR", "nope")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("MH3SP_LMP_ADDR"), "{err}");
        assert!(config(&[("MH3SP_HEALTH_ADDR", "")]).is_err());
    }
}
