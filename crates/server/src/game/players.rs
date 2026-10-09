//! How connections reach each other: each one has a [`Mailbox`]; logged-in players are found by Capcom ID in the
//! registry (decision 4). The socket stays with its connection task, the only writer: frames for a player go
//! through its mailbox.

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dashmap::DashMap;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use wire::Frame;

/// Frames waiting for a connection's task to write them to its socket (then the system's TCP buffer absorbs a slow
/// console). A player receives under 1 frame/s, ~30/s at the busiest in a full city, ~80 bytes each: a mailbox only
/// fills once the console has stopped reading for about a minute. A memory guard, not a latency rule.
const MAILBOX_SIZE: usize = 256;

/// A connection's mailbox: frames to send it, and a way to drop it.
#[derive(Clone, Debug)]
pub struct Mailbox {
    /// The internal connection number (tells two logins of the same Capcom ID apart).
    pub conn: u64,
    /// The console's address (its connection's): NAT negotiation is for logged-in consoles only.
    pub ip: IpAddr,
    tx: mpsc::Sender<Frame>,
    kick: CancellationToken,
    /// Messages may wait in the database (from the start: they may have come while away).
    mail: Arc<AtomicBool>,
}

impl Mailbox {
    /// The mailbox, the frames its connection sends, and the signal it watches.
    pub fn new(conn: u64, ip: IpAddr) -> (Self, mpsc::Receiver<Frame>, CancellationToken) {
        let (tx, rx) = mpsc::channel(MAILBOX_SIZE);
        let kick = CancellationToken::new();
        (
            Mailbox {
                conn,
                ip,
                tx,
                kick: kick.clone(),
                mail: Arc::new(AtomicBool::new(true)),
            },
            rx,
            kick,
        )
    }

    /// Queues a frame; a full mailbox drops its connection.
    pub fn send(&self, frame: Frame) {
        if let Err(mpsc::error::TrySendError::Full(_)) = self.tx.try_send(frame) {
            warn!(conn = self.conn, "mailbox full, dropping the connection");
            self.kick.cancel();
        }
    }

    pub fn kick(&self) {
        self.kick.cancel();
    }

    /// Whether messages may wait for this player, now cleared.
    pub fn take_mail(&self) -> bool {
        self.mail.swap(false, Ordering::Relaxed)
    }
}

/// Logged-in players (on FMP), by Capcom ID.
#[derive(Default)]
pub struct Players(DashMap<String, Mailbox>);

impl Players {
    /// Registers a player; the same Capcom ID logged in elsewhere is kicked.
    pub fn register(&self, capcom_id: &str, mailbox: Mailbox) {
        if let Some(previous) = self.0.insert(capcom_id.to_owned(), mailbox) {
            previous.kick();
        }
    }

    /// Sends a frame to a logged-in player: `false` if they're not.
    pub fn send(&self, capcom_id: &str, frame: Frame) -> bool {
        match self.0.get(capcom_id) {
            Some(mailbox) => {
                mailbox.send(frame);
                true
            }
            None => false,
        }
    }

    /// Tells a logged-in player messages wait for them.
    pub fn mail(&self, capcom_id: &str) {
        if let Some(mailbox) = self.0.get(capcom_id) {
            mailbox.mail.store(true, Ordering::Relaxed);
        }
    }

    /// Whether a logged-in player connects from `ip`. ponytail: a scan of every player, per NAT negotiation
    /// started; an index by address if thousands play at once.
    pub fn online(&self, ip: IpAddr) -> bool {
        self.0.iter().any(|m| m.ip == ip)
    }

    /// Unregisters a player, unless another connection took its Capcom ID since.
    pub fn unregister(&self, capcom_id: &str, conn: u64) {
        self.0.remove_if(capcom_id, |_, m| m.conn == conn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_login_kicks_the_first() {
        let players = Players::default();
        let ip = IpAddr::from([127, 0, 0, 1]);
        let (first, _rx1, kicked1) = Mailbox::new(1, ip);
        let (second, _rx2, kicked2) = Mailbox::new(2, ip);
        players.register("ABC123", first);
        players.register("ABC123", second);
        assert!(kicked1.is_cancelled() && !kicked2.is_cancelled());
        // The first connection leaving doesn't unregister the second.
        players.unregister("ABC123", 1);
        assert!(players.0.contains_key("ABC123"));
        players.unregister("ABC123", 2);
        assert!(players.0.is_empty());
    }
}
