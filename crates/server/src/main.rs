//! `mh3sp`              run the server
//! `mh3sp healthcheck`  exit 0 if a running server is live (container healthcheck)
//! `mh3sp announce "<text>"`  an announcement to every player of the running server
//! `mh3sp stop <delay|cancel>`  stop the running server with warning (`5min`, `1h30m`, `30s`), or not
//! `mh3sp bot add <Capcom ID>`  a quest bot joins that player's party (needs the brain, `BOT_PORT`; the `bot` feature)
//! `mh3sp bot remove <bot>`    the bot leaves
//! `mh3sp maintenance "<message>"`  planned maintenance: OPN only, every console refused with the message,
//!                      no database
//!
//! Configuration: environment variables, see `server::config`.

use anyhow::Result;
use server::Config;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    // Dev convenience: `.env` fills in variables that aren't already set.
    let _ = dotenvy::dotenv();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let maintenance = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        [] => None,
        ["healthcheck"] => return server::health::check(server::config::health()?),
        ["announce", text] => {
            println!(
                "{}",
                server::health::announce(server::config::health()?, text)?
            );
            return Ok(());
        }
        ["stop", delay] => {
            println!(
                "{}",
                server::health::stop(server::config::health()?, delay)?
            );
            return Ok(());
        }
        #[cfg(feature = "bot")]
        ["bot", verb @ ("add" | "remove"), who] => {
            println!(
                "{}",
                server::health::bot(server::config::health()?, verb, who)?
            );
            return Ok(());
        }
        ["maintenance", message] => Some(message.to_owned()),
        _ => anyhow::bail!(
            "usage: mh3sp [healthcheck | announce \"<text>\" | stop <delay|cancel> | bot add|remove <id> | maintenance \"<message>\"]"
        ),
    };

    tracing_subscriber::fmt()
        // Colours on a terminal only: not in `docker logs` or log collectors.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn")),
        )
        .init();
    let mut config = Config::from_env()?;
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
