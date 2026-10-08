//! How connections reach each other: each one has a [`Mailbox`]; logged-in players are found by Capcom ID in the
//! registry (decision 4). The socket stays with its connection task, the only writer. Frames for other players
//! (a bounded channel per mailbox) come with the first notification sent to someone else.

use dashmap::DashMap;
use tokio_util::sync::CancellationToken;

/// A connection's mailbox: a way to drop it.
#[derive(Clone, Debug)]
pub struct Mailbox {
    /// The internal connection number (tells two logins of the same Capcom ID apart).
    pub conn: u64,
    kick: CancellationToken,
}

impl Mailbox {
    /// The mailbox, and the signal its connection watches.
    pub fn new(conn: u64) -> (Self, CancellationToken) {
        let kick = CancellationToken::new();
        (
            Mailbox {
                conn,
                kick: kick.clone(),
            },
            kick,
        )
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
        let (first, kicked1) = Mailbox::new(1);
        let (second, kicked2) = Mailbox::new(2);
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
