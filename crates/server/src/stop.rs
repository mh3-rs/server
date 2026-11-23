//! Stopping the server with warning (M5 D4): `mh3sp stop 5min` announces it, reminds players at fixed steps, then
//! stops the server; nothing is refused meanwhile (play goes on as usual until the end). `mh3sp stop cancel` lifts it.
//! SIGTERM runs it too, shortened (`docker stop` waits 10 s). Maintenance is only a way to start (`mh3sp
//! maintenance`: a placeholder refusing everyone).

use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::Shared;
use crate::game::announce;

/// Reminders, when this long is left (those shorter than the delay asked).
const STEPS: [u64; 8] = [3600, 1800, 900, 300, 60, 30, 10, 5];

/// Starts stopping the server in `delay` (a stop already under way is replaced, quietly).
pub fn begin(shared: &Arc<Shared>, delay: Duration) {
    let cancel = shared.stop.start();
    info!(seconds = delay.as_secs(), "stopping");
    announce(shared, &format!("Server stopping in {}.", human(delay)));
    let shared = shared.clone();
    // On the shared tracker, which `Running::stop` waits for: the countdown ends with the server too (`shutdown`).
    shared.clone().tasks.spawn(async move {
        let end = Instant::now() + delay;
        let steps = STEPS.map(Duration::from_secs);
        for step in steps.into_iter().filter(|&s| s < delay) {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = shared.shutdown.cancelled() => return,
                _ = tokio::time::sleep_until(end - step) => {
                    announce(&shared, &format!("Server stopping in {}.", human(step)));
                }
            }
        }
        // A cancel that comes in time always wins: first in the select, and again under the lock `end` takes.
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {}
            _ = shared.shutdown.cancelled() => {}
            _ = tokio::time::sleep_until(end) => {
                if shared.stop.end(&cancel, &shared.shutdown) {
                    info!("stopped as asked");
                }
            }
        }
    });
}

/// Cancels a stop under way, and says so to the players, if there was one in time.
pub fn cancel(shared: &Shared) -> Cancelled {
    let cancelled = shared.stop.cancel(&shared.shutdown);
    if cancelled == Cancelled::Lifted {
        info!("stop cancelled");
        announce(shared, "Server stop cancelled.");
    }
    cancelled
}

/// What `cancel` did.
#[derive(Debug, PartialEq, Eq)]
pub enum Cancelled {
    /// The stop under way is lifted.
    Lifted,
    /// The countdown ended: the server is stopping.
    TooLate,
    /// No stop was under way.
    Nothing,
}

impl Cancelled {
    /// What the operator and the admins are told.
    pub fn said(&self) -> &'static str {
        match self {
            Cancelled::Lifted => "stop cancelled",
            Cancelled::TooLate => "too late, the server is stopping",
            Cancelled::Nothing => "no stop to cancel",
        }
    }
}

/// The stop under way, if any: the token that lifts it. Every change is made under its lock, so a countdown that
/// ends and a cancel that comes can't both think they won.
#[derive(Default)]
pub struct Stop(std::sync::Mutex<Option<CancellationToken>>);

impl Stop {
    /// A stop begins: the one under way, if any, is lifted (quietly) and can't end after this.
    fn start(&self) -> CancellationToken {
        let token = CancellationToken::new();
        let mut stop = self.0.lock().unwrap();
        if let Some(previous) = stop.replace(token.clone()) {
            previous.cancel();
        }
        token
    }

    /// The countdown of `token` ended: whether the server stops now. Not if a cancel or a newer stop lifted this one
    /// first (it is cancelled then, under this lock); otherwise it is the stop under way, and it is over.
    fn end(&self, token: &CancellationToken, shutdown: &CancellationToken) -> bool {
        let mut stop = self.0.lock().unwrap();
        if token.is_cancelled() {
            return false;
        }
        stop.take();
        shutdown.cancel();
        true
    }

    /// Lifts the stop under way, if its countdown hasn't ended; `shutdown` is cancelled once it has.
    fn cancel(&self, shutdown: &CancellationToken) -> Cancelled {
        let mut stop = self.0.lock().unwrap();
        match stop.take() {
            Some(token) => {
                token.cancel();
                Cancelled::Lifted
            }
            None if shutdown.is_cancelled() => Cancelled::TooLate,
            None => Cancelled::Nothing,
        }
    }
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
    fn a_cancel_in_time_lifts_the_stop_and_its_countdown_cannot_end() {
        let (stop, shutdown) = (Stop::default(), CancellationToken::new());
        let token = stop.start();
        assert_eq!(stop.cancel(&shutdown), Cancelled::Lifted);
        assert!(token.is_cancelled());
        // The countdown reaches its end after that: it stops nothing.
        assert!(!stop.end(&token, &shutdown));
        assert!(!shutdown.is_cancelled());
        assert_eq!(stop.cancel(&shutdown), Cancelled::Nothing);
    }

    #[test]
    fn a_cancel_after_the_countdown_ended_is_too_late() {
        let (stop, shutdown) = (Stop::default(), CancellationToken::new());
        let token = stop.start();
        assert!(stop.end(&token, &shutdown));
        assert!(shutdown.is_cancelled());
        // Not "cancelled": the server is stopping.
        assert_eq!(stop.cancel(&shutdown), Cancelled::TooLate);
        assert!(!token.is_cancelled());
    }

    #[test]
    fn a_cancel_with_no_stop_says_so() {
        let (stop, shutdown) = (Stop::default(), CancellationToken::new());
        assert_eq!(stop.cancel(&shutdown), Cancelled::Nothing);
    }

    #[test]
    fn a_newer_stop_keeps_its_countdown_when_the_older_one_ends() {
        let (stop, shutdown) = (Stop::default(), CancellationToken::new());
        let older = stop.start();
        let newer = stop.start();
        assert!(older.is_cancelled() && !newer.is_cancelled());
        // The older one's timer fires anyway: it neither stops the server nor takes the newer one's token.
        assert!(!stop.end(&older, &shutdown));
        assert!(!shutdown.is_cancelled());
        assert_eq!(stop.cancel(&shutdown), Cancelled::Lifted);
        assert!(newer.is_cancelled());
    }

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
