//! `mh3sp`              run the server
//! `mh3sp healthcheck`  exit 0 if a running server is live (container healthcheck)
//! `mh3sp announce "<text>"`  an announcement to every player of the running server
//! `mh3sp stop <delay|cancel>`  stop the running server with warning (`5min`, `1h30m`, `30s`), or not
//! `mh3sp bot add <Capcom ID>`  a quest bot joins that player's party (needs the brain, `bot_port`; the `bot` feature)
//! `mh3sp bot remove <bot>`    the bot leaves
//! `mh3sp user add <email> <admin|player>`  a user of the API, and a token for the CLI (printed once)
//! `mh3sp user password <email>`  sets that user's password (read from stdin), for the website's sign-in
//! `mh3sp token add <email>`   another CLI token for that user (printed once)
//! `mh3sp token revoke <key>`  revokes a token (its key: the part before the dot)
//! `mh3sp patch <dir>`      the players' patch for this deployment (`domain`, the CA in `tls_dir`, created if
//!                      missing), and the DNS records it needs
//! `mh3sp maintenance "<message>"`  planned maintenance: OPN only, every console refused with the message,
//!                      no database
//!
//! Configuration: `mh3sp.toml`, see `server::config`.

use anyhow::Result;
use server::Config;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let maintenance = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        [] => None,
        ["healthcheck"] => return server::health::check(Config::load()?.health),
        ["announce", text] => {
            println!(
                "{}",
                server::health::announce(Config::load()?.health, text)?
            );
            return Ok(());
        }
        ["stop", delay] => {
            println!("{}", server::health::stop(Config::load()?.health, delay)?);
            return Ok(());
        }
        #[cfg(feature = "bot")]
        ["bot", verb @ ("add" | "remove"), who] => {
            println!(
                "{}",
                server::health::bot(Config::load()?.health, verb, who)?
            );
            return Ok(());
        }
        ["user", "add", email, role] => {
            let role = server::users::Role::parse(role)
                .ok_or_else(|| anyhow::anyhow!("role: admin or player"))?;
            let db = database().await?;
            server::users::add(&db, email, role).await?;
            println!("{}", server::users::issue(&db, email, "cli").await?);
            return Ok(());
        }
        ["user", "password", email] => {
            let mut password = String::new();
            std::io::stdin().read_line(&mut password)?;
            let password = password.trim_end_matches(['\n', '\r']);
            let db = database().await?;
            anyhow::ensure!(
                server::users::set_password(&db, email, password).await?,
                "no user {email}"
            );
            println!("password set");
            return Ok(());
        }
        ["token", "add", email] => {
            println!(
                "{}",
                server::users::issue(&database().await?, email, "cli").await?
            );
            return Ok(());
        }
        ["token", "revoke", key] => {
            let revoked =
                server::users::revoke(&database().await?, key, "revoked by the operator").await?;
            anyhow::ensure!(revoked, "no valid token {key}");
            println!("revoked");
            return Ok(());
        }
        ["patch", dir] => {
            let config = Config::load()?;
            let domain = config
                .domain
                .ok_or_else(|| anyhow::anyhow!("domain: unset in the configuration"))?;
            let records = server::patch::write(dir.as_ref(), &domain, &config.tls_dir)?;
            println!(
                "Written in {dir}: riivolution/ (Dolphin, a Wii booting the disc), wit/ (an image for USB loaders), codes/ (USB loaders with Ocarina). Players can also download them from http://{domain}/patch."
            );
            println!(
                "DNS records, each pointing to this server ({}):",
                config
                    .external_address
                    .as_deref()
                    .unwrap_or("external_address")
            );
            for name in records {
                println!("  {name}");
            }
            return Ok(());
        }
        ["maintenance", message] => Some(message.to_owned()),
        _ => anyhow::bail!(
            "usage: mh3sp [healthcheck | announce \"<text>\" | stop <delay|cancel> | bot add|remove <id> | user add <email> <role> | user password <email> | token add <email> | token revoke <key> | patch <dir> | maintenance \"<message>\"]"
        ),
    };

    tracing_subscriber::fmt()
        // Colours on a terminal only: not in `docker logs` or log collectors.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn")),
        )
        .init();
    let mut config = Config::load()?;
    config.maintenance = maintenance;
    let running = server::start(&config).await?;
    // A signal (`docker stop`, which waits 10 s) stops with a short warning; a second one, or a `mh3sp stop` coming to
    // its end, stops now.
    let stopping = running.stopping();
    tokio::select! {
        _ = shutdown_signal() => {
            running.stop_in(SIGNAL_WARNING);
            tokio::select! {
                _ = shutdown_signal() => {}
                _ = stopping.cancelled() => {}
            }
        }
        _ = stopping.cancelled() => {}
    }
    tracing::info!("shutting down");
    running.stop().await;
    Ok(())
}

/// The configuration's database, opened (and migrated) for a command.
async fn database() -> Result<sqlx::SqlitePool> {
    server::database::open(&Config::load()?.database_url).await
}

/// How long a signal's stop warns players (under `docker stop`'s 10 s).
const SIGNAL_WARNING: std::time::Duration = std::time::Duration::from_secs(5);

/// Ctrl-C, or SIGTERM (what `docker stop` sends).
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = ctrl_c.await;
}
