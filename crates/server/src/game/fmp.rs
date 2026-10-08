//! FMP (port 8202): the world servers. The console comes twice with the ticket of its last LMP visit: once to pick a
//! world server, then to play on it (gates, cities, quests).

use anyhow::anyhow;
use tracing::{Span, info};
use wire::packets::*;
use wire::{Frame, Item};

use super::players::Mailbox;
use super::{Fault, Handled, Session, reply, server_time, unexpected};
use crate::Shared;

/// `NtcLogin` status on FMP.
const LOGGED_IN: u8 = 3;

pub struct Fmp {
    mailbox: Mailbox,
    /// The session and the ticket it came with (it goes back under the same one: FMP issues none).
    login: Option<(Session, String)>,
}

impl Fmp {
    pub fn new(mailbox: Mailbox) -> Self {
        Fmp {
            mailbox,
            login: None,
        }
    }

    pub async fn handle(&mut self, shared: &Shared, req: &Frame) -> Handled {
        let (seq, p) = (req.seq, &req.payload);
        match (&self.login, req.id) {
            (None, AnsConnection::ID) => {
                let data = AnsConnection::decode(p)?.data;
                let Some(Item::Binary(ticket) | Item::String(ticket)) = data.get(0x02) else {
                    return Err(Fault::Client(anyhow!("no ticket")));
                };
                let ticket = String::from_utf8_lossy(ticket).into_owned();
                let session = shared
                    .tickets
                    .remove(&ticket)
                    .ok_or_else(|| Fault::Client(anyhow!("unknown or expired ticket")))?;
                let capcom_id = session
                    .capcom_id
                    .clone()
                    .ok_or_else(|| Fault::Client(anyhow!("no Capcom ID chosen")))?;
                let span = Span::current();
                span.record(
                    "console",
                    tracing::field::display(session.console.public_id),
                );
                span.record("capcom_id", capcom_id.as_str());
                shared.players.register(&capcom_id, self.mailbox.clone());
                info!("joined FMP");
                self.login = Some((session, ticket));
                reply(seq, &NtcLogin { status: LOGGED_IN })
            }
            (Some(_), ReqServerTime::ID) => {
                ReqServerTime::decode(p)?;
                server_time(seq)
            }
            (_, id) => Err(unexpected(id)),
        }
    }

    /// The console leaves for its next connection: the session waits under its ticket.
    pub fn leave(&mut self, shared: &Shared) {
        if let Some((session, ticket)) = self.login.take() {
            self.unregister(shared, &session);
            shared.tickets.insert(ticket, session);
        }
    }

    /// The connection is closed.
    pub fn close(&mut self, shared: &Shared) {
        if let Some((session, _)) = self.login.take() {
            self.unregister(shared, &session);
        }
    }

    fn unregister(&self, shared: &Shared, session: &Session) {
        if let Some(capcom_id) = &session.capcom_id {
            shared.players.unregister(capcom_id, self.mailbox.conn);
        }
    }
}
