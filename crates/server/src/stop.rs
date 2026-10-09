//! Stopping the server with warning (M5 D4): `mh3sp stop 5min` announces it, turns maintenance on at once (no new
//! logins, no new quests; everything else works on), reminds players at fixed steps, then stops the server.
//! `mh3sp stop cancel` lifts it. SIGTERM runs it too, shortened (`docker stop` waits 10 s).

use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::Shared;
use crate::game::announcement;

/// Reminders, when this long is left (those shorter than the delay asked).
const STEPS: [u64; 8] = [3600, 1800, 900, 300, 60, 30, 10, 5];
/// The maintenance message while stopping (OPN shows it to consoles logging in).
const MAINTENANCE: &str = "The server is restarting. Please come back in a few minutes.";

/// Starts stopping the server in `delay` (a stop already under way is replaced, quietly).
pub fn begin(shared: &Arc<Shared>, delay: Duration) {
    let cancel = CancellationToken::new();
    if let Some(previous) = shared.stop.lock().unwrap().replace(cancel.clone()) {
        previous.cancel();
    }
    info!(seconds = delay.as_secs(), "stopping");
    announce(shared, &format!("Server stopping in {}.", human(delay)));
    shared.set_maintenance(Some(MAINTENANCE.to_owned()));
    let shared = shared.clone();
    tokio::spawn(async move {
        let end = Instant::now() + delay;
        let steps = STEPS.map(Duration::from_secs);
        for step in steps.into_iter().filter(|&s| s < delay) {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep_until(end - step) => {
                    announce(&shared, &format!("Server stopping in {}.", human(step)));
                }
            }
        }
        tokio::select! {
            _ = cancel.cancelled() => {}
            _ = tokio::time::sleep_until(end) => {
                info!("stopped as asked");
                shared.shutdown.cancel();
            }
        }
    });
}

/// Cancels a stop under way: `false` if there was none.
pub fn cancel(shared: &Shared) -> bool {
    let Some(stop) = shared.stop.lock().unwrap().take() else {
        return false;
    };
    stop.cancel();
    shared.set_maintenance(shared.config.maintenance.clone());
    info!("stop cancelled");
    announce(shared, "Server stop cancelled.");
    true
}

fn announce(shared: &Shared, text: &str) {
    shared.players.broadcast(&announcement(text));
}

/// A delay as people write it: `5min`, `1h30m`, `30s`, `2 hours`; `None` if it isn't one (or is 0).
pub fn parse(text: &str) -> Option<Duration> {
    let mut seconds = 0u64;
    let mut rest = text.trim();
    while !rest.is_empty() {
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        let number: u64 = rest[..digits].parse().ok()?;
        rest = rest[digits..].trim_start();
        let letters = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        let unit = match &rest[..letters] {
            "s" | "sec" | "secs" | "second" | "seconds" => 1,
            "m" | "min" | "mins" | "minute" | "minutes" => 60,
            "h" | "hour" | "hours" => 3600,
            _ => return None,
        };
        seconds = seconds.checked_add(number.checked_mul(unit)?)?;
        rest = rest[letters..].trim_start();
    }
    (seconds > 0).then(|| Duration::from_secs(seconds))
}

/// `1 hour 30 minutes`, `5 minutes`, `1 second`.
pub fn human(delay: Duration) -> String {
    let s = delay.as_secs();
    let parts = [
        (s / 3600, "hour"),
        (s % 3600 / 60, "minute"),
        (s % 60, "second"),
    ];
    let words: Vec<String> = parts
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, unit)| format!("{n} {unit}{}", if *n == 1 { "" } else { "s" }))
        .collect();
    match words.is_empty() {
        true => "0 seconds".to_owned(),
        false => words.join(" "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_as_people_write_them() {
        let secs = |t| parse(t).map(|d| d.as_secs());
        assert_eq!(secs("5min"), Some(300));
        assert_eq!(secs("1h30m"), Some(5400));
        assert_eq!(secs("2 hours"), Some(7200));
        assert_eq!(secs("30s"), Some(30));
        assert_eq!(secs("0s"), None);
        assert_eq!(secs("soon"), None);
        assert_eq!(secs("5"), None);
        assert_eq!(human(Duration::from_secs(5400)), "1 hour 30 minutes");
        assert_eq!(human(Duration::from_secs(60)), "1 minute");
    }
}
