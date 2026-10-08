//! `mh3sp [config.toml]`            run the server
//! `mh3sp healthcheck [config.toml]` exit 0 if a running server is live (container healthcheck)

use std::path::PathBuf;

use anyhow::Result;
use server::Config;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1).peekable();
    let healthcheck = args.next_if_eq("healthcheck").is_some();
    let path = PathBuf::from(args.next().unwrap_or_else(|| "config.toml".into()));
    let config = Config::load(&path)?;

    if healthcheck {
        return server::health::check(config.listen.health);
    }

    tracing_subscriber::fmt()
        // Colours on a terminal only: not in `docker logs` or log collectors.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn")),
        )
        .init();
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
