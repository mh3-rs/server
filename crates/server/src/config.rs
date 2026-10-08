//! Server configuration. Ports and binding are fixed (`0.0.0.0`, OPN 8200, LMP 8201, FMP 8202, health 8280):
//! the container's port mapping is the way to change them. The rest comes from environment variables, each with a
//! default; a `.env` file is loaded first when present (dev).
//!
//! | Variable | Default |
//! |---|---|
//! | `TLS_CERT`, `TLS_KEY` | `certs/dev/server.crt`, `certs/dev/server.key` (signed by the CA the patched game trusts) |
//! | `DATABASE_URL` | `sqlite://data/mh3sp.db` (the name sqlx uses) |
//! | `RUST_LOG` | `info,sqlx=warn` (read by the logger, not here) |
//! | `EXTERNAL_ADDRESS` | none: the address consoles reach this server at, handed out for LMP and FMP (never guessed) |
//! | `NATNEG_HOSTNAME` | none: the only NAT negotiation hostname accepted in a disc's memory (our patch's) |
//!
//! Without `EXTERNAL_ADDRESS` or `NATNEG_HOSTNAME` the server runs but refuses every console ("not configured").

use std::net::SocketAddr;
use std::path::PathBuf;

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
    /// Maintenance mode (`mh3sp maintenance "<message>"`, not an environment variable): OPN only, every console
    /// refused with this message, no database.
    pub maintenance: Option<String>,
}

impl Config {
    pub fn from_env() -> Self {
        let get =
            |key: &str, default: &str| std::env::var(key).unwrap_or_else(|_| default.to_owned());
        let optional = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
        Self {
            opn: SocketAddr::from(([0, 0, 0, 0], 8200)),
            lmp: SocketAddr::from(([0, 0, 0, 0], 8201)),
            fmp: SocketAddr::from(([0, 0, 0, 0], 8202)),
            health: SocketAddr::from(([0, 0, 0, 0], 8280)),
            tls_cert: get("TLS_CERT", "certs/dev/server.crt").into(),
            tls_key: get("TLS_KEY", "certs/dev/server.key").into(),
            database_url: get("DATABASE_URL", "sqlite://data/mh3sp.db"),
            external_address: optional("EXTERNAL_ADDRESS"),
            natneg_hostname: optional("NATNEG_HOSTNAME"),
            maintenance: None,
        }
    }
}
