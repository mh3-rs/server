//! `mh3sp`              run the server
//! `mh3sp healthcheck`  exit 0 if a running server is live (container healthcheck)
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
        ["maintenance", message] => Some(message.to_owned()),
        _ => anyhow::bail!("usage: mh3sp [healthcheck | maintenance \"<message>\"]"),
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
    shutdown_signal().await;
    tracing::info!("shutting down");
    running.stop().await;
    Ok(())
}

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
