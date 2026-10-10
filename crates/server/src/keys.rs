//! Encryption keys between two connections (`../docs/history/packet encryption.md`, E2). Each console gets its own key at
//! login (OPN); every later connection of that console is encrypted with it from its first frame. A socket says
//! nothing about who connects, and the server speaks first, already encrypted. So when a connection ends with
//! `ReqShut` its key waits here with the console's IP; the next connection from that IP opens with the newest key
//! waiting for it (`ReqConnection`: the right one in the usual case; with several consoles in transit at one IP it
//! may be another's, which the console tolerates, it ignores the value), and its first frame (`AnsConnection`) is
//! decrypted with the keys waiting for that IP only: usually one, more when consoles behind one router switch servers
//! within the same few seconds. The key that reads as a valid `AnsConnection` goes with the connection.

use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bytes::Bytes;
use wire::crypt::{self, KEY_LEN};
use wire::packets::{AnsConnection, Packet};

pub type Key = [u8; KEY_LEN];

/// How long a key waits for its console's next connection: the ticket hand-off's time (decision 4).
const IN_TRANSIT: Duration = Duration::from_secs(120);

/// Keys in transit. ponytail: a list scanned per connection; it holds only the consoles switching servers within the
/// last two minutes.
#[derive(Default)]
pub struct Keys(Mutex<Vec<(IpAddr, Instant, Key)>>);

impl Keys {
    pub fn draw() -> Result<Key, openssl::error::ErrorStack> {
        let mut key = [0; KEY_LEN];
        openssl::rand::rand_bytes(&mut key)?;
        Ok(key)
    }

    /// A console at `ip` leaves for its next connection.
    pub fn put(&self, ip: IpAddr, key: Key) {
        self.0.lock().unwrap().push((ip, Instant::now(), key));
    }

    /// The key most likely to be the console's at `ip` (the newest waiting for it), for the connection's opening.
    pub fn likely(&self, ip: IpAddr) -> Option<Key> {
        let mut keys = self.0.lock().unwrap();
        keys.retain(|(_, at, _)| at.elapsed() < IN_TRANSIT);
        keys.iter()
            .rev()
            .find(|(from, _, _)| *from == ip)
            .map(|(_, _, key)| *key)
    }

    /// The key of the console at `ip` whose first frame this is, taken out, and the frame's payload decrypted.
    pub fn take(&self, ip: IpAddr, first: &[u8]) -> Option<(Key, Bytes)> {
        let mut keys = self.0.lock().unwrap();
        keys.retain(|(_, at, _)| at.elapsed() < IN_TRANSIT);
        let (i, plain) = keys
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, (from, _, key))| {
                if *from != ip {
                    return None;
                }
                let plain = crypt::decrypt(key, first).ok()?;
                AnsConnection::decode(&plain).is_ok().then_some((i, plain))
            })?;
        Some((keys.remove(i).2, plain))
    }
}
