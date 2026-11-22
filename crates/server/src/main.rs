//! `mh3sp`              run the server
//! `mh3sp healthcheck`  exit 0 if a running server is live (container healthcheck)
//! `mh3sp announce "<text>"`  an announcement to every player of the running server
//! `mh3sp stop <delay|cancel>`  stop the running server with warning (`5min`, `1h30m`, `30s`), or not
//! `mh3sp patch <dir>`      the players' patch for this deployment (`domain`, the CA in `tls_dir`, created if
//!                      missing), and the DNS records it needs
//! `mh3sp check <domain>`   from another machine: which port forward or DNS record is missing (reads no configuration;
//!                      exit 1 if any check fails)
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
        // Before anything reads the configuration: it must answer anywhere, with no `mh3sp.toml`.
        ["--version"] => {
            println!("mh3sp {}", server::VERSION);
            return Ok(());
        }
        ["healthcheck"] => return server::public::check(Config::load()?.public),
        ["announce", text] => {
            println!(
                "{}",
                server::control::announce(server::control::NAME, text)?
            );
            return Ok(());
        }
        ["stop", delay] => {
            println!("{}", server::control::stop(server::control::NAME, delay)?);
            return Ok(());
        }
        ["patch", dir] => {
            let config = Config::load()?;
            let domain = config
                .domain
                .ok_or_else(|| anyhow::anyhow!("domain: unset in the configuration"))?;
            let records = server::patch::write(dir.as_ref(), &domain, &config.tls_dir)?;
            println!(
                "Written in {dir}: riivolution/ (Dolphin, a Wii booting the disc), wit/ (an image for USB loaders), codes/ (USB loaders with Ocarina). Players can also download them from the public port: http://{domain}:{}/patch.",
                config.public.port()
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
        ["check", domain] => {
            // The same rule as the configuration's: it also keeps the name out of the probe's HTTP request.
            let domain = server::config::domain(domain)?;
            let report = server::check::run(&domain).await;
            println!("{report}");
            std::process::exit(i32::from(!report.passed()));
        }
        ["maintenance", message] => Some(message.to_owned()),
        _ => anyhow::bail!(
            "usage: mh3sp [--version | healthcheck | announce \"<text>\" | stop <delay|cancel> | patch <dir> | check <domain> | maintenance \"<message>\"]"
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
