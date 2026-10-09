//! How connections reach each other: each one has a [`Mailbox`]; logged-in players are found by Capcom ID in the
//! registry (decision 4). The socket stays with its connection task, the only writer: frames for a player go
//! through its mailbox.

use dashmap::DashMap;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use wire::Frame;

/// Frames waiting for a connection; a player this far behind is dropped rather than slowing the others.
const MAILBOX_SIZE: usize = 256;

/// A connection's mailbox: frames to send it, and a way to drop it.
#[derive(Clone, Debug)]
pub struct Mailbox {
    /// The internal connection number (tells two logins of the same Capcom ID apart).
    pub conn: u64,
    tx: mpsc::Sender<Frame>,
    kick: CancellationToken,
}

impl Mailbox {
    /// The mailbox, the frames its connection sends, and the signal it watches.
    pub fn new(conn: u64) -> (Self, mpsc::Receiver<Frame>, CancellationToken) {
        let (tx, rx) = mpsc::channel(MAILBOX_SIZE);
        let kick = CancellationToken::new();
        (
            Mailbox {
                conn,
                tx,
                kick: kick.clone(),
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

    /// Sends a frame to a logged-in player (nothing if it's gone).
    pub fn send(&self, capcom_id: &str, frame: Frame) {
        if let Some(mailbox) = self.0.get(capcom_id) {
            mailbox.send(frame);
        }
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
        let (first, _rx1, kicked1) = Mailbox::new(1);
        let (second, _rx2, kicked2) = Mailbox::new(2);
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
