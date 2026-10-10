//! The quest bots' tunnel, the server's side (`roadmap/experiment-bots.md`, `architecture.md` decision 11): the server
//! is the bots' body, the brain (`mh3sp-brain`) connects to `BOT_PORT` on localhost and decides what they do. One
//! connection carries every bot; a newer brain replaces the older one.

use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use tunnel::{ToBrain, ToServer};

use crate::peer::Event;

use crate::Shared;

/// The connected brain, if any: where commands go.
#[derive(Default)]
pub struct Brain(Mutex<Option<mpsc::UnboundedSender<ToBrain>>>);

impl Brain {
    /// Sends the brain a command: `false` without a brain.
    pub fn send(&self, message: ToBrain) -> bool {
        match self.0.lock().unwrap().as_ref() {
            Some(tx) => tx.send(message).is_ok(),
            None => false,
        }
    }

    pub fn connected(&self) -> bool {
        self.0
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|tx| !tx.is_closed())
    }
}

/// Accepts the brain's connections until `shutdown`.
pub async fn serve(listener: TcpListener, shared: Arc<Shared>, shutdown: CancellationToken) {
    loop {
        let stream = tokio::select! {
            _ = shutdown.cancelled() => return,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(e) => {
                    warn!(error = %e, "bot port: accept failed");
                    continue;
                }
            },
        };
        info!("brain connected");
        let (tx, mut rx) = mpsc::unbounded_channel();
        *shared.brain.0.lock().unwrap() = Some(tx);
        let mut framed = Framed::new(stream, tunnel::codec());
        let shared = shared.clone();
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => return,
                    command = rx.recv() => {
                        let Some(command) = command else { return };
                        let Ok(frame) = tunnel::encode(&command) else { continue };
                        if framed.send(frame).await.is_err() {
                            break;
                        }
                    }
                    frame = framed.next() => match frame {
                        Some(Ok(frame)) => match tunnel::decode::<ToServer>(&frame) {
                            Ok(action) => act(&shared, action),
                            Err(e) => warn!(error = %e, "brain: unreadable message"),
                        },
                        _ => break,
                    },
                }
            }
            info!("brain disconnected");
        });
    }
}

/// What the brain asks a bot to do: its task plays it on the wire (`peer.rs`).
fn act(shared: &Shared, action: ToServer) {
    debug!(?action, "brain action");
    let (bot, event) = match action {
        ToServer::Move { bot, position, run } => (bot, Event::Move { position, run }),
        ToServer::ChangeArea { bot, area } => (bot, Event::Area { area }),
    };
    shared.peers.deliver(&bot, event);
}
