//! Consoles and their Capcom ID slots, in the database.
//!
//! A console is known by its online support code, which the server stores only as HMAC-SHA256(secret, code): the
//! code is a credential, and a leaked database alone reveals none. Generated ids use the game's alphabet
//! (`0-9A-Z`) and a cryptographic random source.

use anyhow::{Context, Result, bail};
use openssl::hash::MessageDigest;
use openssl::pkey::PKey;
use openssl::sign::Signer;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::config::Secret;
use crate::game::profile::Profile;

/// Support codes and PAT tickets are 11 characters, Capcom IDs 6 (D7).
pub const SUPPORT_CODE_LEN: usize = 11;
const CAPCOM_ID_LEN: usize = 6;
/// Collisions are rare (36^6 Capcom IDs): a few attempts, then it's a bug.
const ATTEMPTS: usize = 10;

/// `len` characters of `0-9A-Z`, uniformly drawn.
pub fn random_id(len: usize) -> String {
    const ALPHABET: &[u8; 36] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut id = String::with_capacity(len);
    let mut buf = [0u8; 32];
    while id.len() < len {
        openssl::rand::rand_bytes(&mut buf).expect("OpenSSL random source");
        // 252 = 7 × 36: bytes above it would favour the first letters.
        for &b in buf.iter().filter(|&&b| b < 252) {
            if id.len() < len {
                id.push(ALPHABET[usize::from(b % 36)] as char);
            }
        }
    }
    id
}

fn hmac(secret: &Secret, code: &str) -> Vec<u8> {
    let key = PKey::hmac(&secret.0).expect("HMAC key");
    let mut signer = Signer::new(MessageDigest::sha256(), &key).expect("HMAC-SHA256");
    signer.update(code.as_bytes()).expect("HMAC update");
    signer.sign_to_vec().expect("HMAC sign")
}

#[derive(Clone, Copy, Debug)]
pub struct Console {
    pub id: i64,
    pub public_id: Uuid,
}

impl Console {
    fn from_row(id: i64, public_id: &[u8]) -> Result<Self> {
        Ok(Console {
            id,
            public_id: Uuid::from_slice(public_id).context("console public id")?,
        })
    }
}

/// The console with this support code; a code the server doesn't know (a console from before a database reset)
/// registers a new one.
pub async fn console(
    db: &SqlitePool,
    secret: &Secret,
    code: &str,
    region: &str,
) -> Result<Console> {
    let hmac = hmac(secret, code);
    let public_id = Uuid::now_v7();
    let public_id = public_id.as_bytes().as_slice();
    let row = sqlx::query!(
        r#"INSERT INTO consoles (public_id, support_code_hmac, region) VALUES (?1, ?2, ?3)
           ON CONFLICT (support_code_hmac) DO UPDATE SET last_seen_at = CURRENT_TIMESTAMP
           RETURNING id AS "id!", public_id"#,
        public_id,
        hmac,
        region
    )
    .fetch_one(db)
    .await?;
    Console::from_row(row.id, &row.public_id)
}

/// A new console, with the support code issued to it.
pub async fn new_console(
    db: &SqlitePool,
    secret: &Secret,
    region: &str,
) -> Result<(Console, String)> {
    for _ in 0..ATTEMPTS {
        let code = random_id(SUPPORT_CODE_LEN);
        let hmac = hmac(secret, &code);
        let public_id = Uuid::now_v7();
        let public_id = public_id.as_bytes().as_slice();
        let row = sqlx::query!(
            r#"INSERT INTO consoles (public_id, support_code_hmac, region) VALUES (?1, ?2, ?3)
               ON CONFLICT DO NOTHING RETURNING id AS "id!", public_id"#,
            public_id,
            hmac,
            region
        )
        .fetch_optional(db)
        .await?;
        if let Some(row) = row {
            return Ok((Console::from_row(row.id, &row.public_id)?, code));
        }
    }
    bail!("no free support code after {ATTEMPTS} attempts")
}

/// A used Capcom ID slot.
#[derive(Debug)]
pub struct Slot {
    pub slot: u32,
    pub capcom_id: String,
    pub name: Vec<u8>,
}

pub async fn slots(db: &SqlitePool, console: &Console) -> Result<Vec<Slot>> {
    Ok(sqlx::query_as!(
        Slot,
        r#"SELECT slot AS "slot: u32", capcom_id, name FROM hunters WHERE console_id = ?1 ORDER BY slot"#,
        console.id
    )
    .fetch_all(db)
    .await?)
}

/// Plays `slot` as the hunter `name`: the slot's Capcom ID, created for an empty slot (`true`). A used slot takes
/// the name given: another hunter on the slot overwrites the previous one, the Capcom ID stays.
pub async fn use_slot(
    db: &SqlitePool,
    console: &Console,
    slot: u32,
    name: &[u8],
) -> Result<(String, bool)> {
    for _ in 0..ATTEMPTS {
        let used = sqlx::query!(
            r#"SELECT id AS "id!", capcom_id, name FROM hunters WHERE console_id = ?1 AND slot = ?2"#,
            console.id,
            slot
        )
        .fetch_optional(db)
        .await?;
        if let Some(used) = used {
            let mut tx = db.begin().await?;
            if used.name != name {
                // Another hunter on the slot: its profile and social links go (the Capcom ID and its moderation
                // records stay).
                sqlx::query!("DELETE FROM hunter_profiles WHERE hunter_id = ?1", used.id)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query!(
                    "DELETE FROM friends WHERE hunter_a = ?1 OR hunter_b = ?1",
                    used.id
                )
                .execute(&mut *tx)
                .await?;
                sqlx::query!(
                    "DELETE FROM friend_requests WHERE from_hunter = ?1 OR to_hunter = ?1",
                    used.id
                )
                .execute(&mut *tx)
                .await?;
                sqlx::query!(
                    "DELETE FROM blacklist WHERE blocker = ?1 OR blocked = ?1",
                    used.id
                )
                .execute(&mut *tx)
                .await?;
            }
            sqlx::query!(
                "UPDATE hunters SET name = ?2, last_seen_at = CURRENT_TIMESTAMP WHERE id = ?1",
                used.id,
                name
            )
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok((used.capcom_id, false));
        }
        let capcom_id = random_id(CAPCOM_ID_LEN);
        let public_id = Uuid::now_v7();
        let public_id = public_id.as_bytes().as_slice();
        // A taken Capcom ID, or the slot filled meanwhile: try again.
        let created = sqlx::query!(
            r#"INSERT INTO hunters (public_id, console_id, slot, capcom_id, name) VALUES (?1, ?2, ?3, ?4, ?5)
               ON CONFLICT DO NOTHING"#,
            public_id,
            console.id,
            slot,
            capcom_id,
            name
        )
        .execute(db)
        .await?;
        if created.rows_affected() == 1 {
            return Ok((capcom_id, true));
        }
    }
    bail!("no free Capcom ID after {ATTEMPTS} attempts")
}

/// A hunter known by its Capcom ID: internal id and name.
pub async fn hunter(db: &SqlitePool, capcom_id: &str) -> Result<Option<(i64, Vec<u8>)>> {
    Ok(sqlx::query!(
        r#"SELECT id AS "id!", name FROM hunters WHERE capcom_id = ?1"#,
        capcom_id
    )
    .fetch_optional(db)
    .await?
    .map(|r| (r.id, r.name)))
}

/// Records a friend request (a new one from the same hunter replaces its message).
pub async fn request_friend(db: &SqlitePool, from: i64, to: i64, message: &[u8]) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO friend_requests (from_hunter, to_hunter, message) VALUES (?1, ?2, ?3)
           ON CONFLICT (from_hunter, to_hunter) DO UPDATE SET message = ?3, created_at = CURRENT_TIMESTAMP"#,
        from,
        to,
        message
    )
    .execute(db)
    .await?;
    Ok(())
}

/// Answers the request `from` sent `to`: `false` if there was none. Accepted, the two become friends.
pub async fn answer_friend_request(
    db: &SqlitePool,
    from: i64,
    to: i64,
    accept: bool,
) -> Result<bool> {
    let mut tx = db.begin().await?;
    let found = sqlx::query!(
        "DELETE FROM friend_requests WHERE from_hunter = ?1 AND to_hunter = ?2",
        from,
        to
    )
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if found && accept {
        let (a, b) = (from.min(to), from.max(to));
        sqlx::query!(
            "INSERT INTO friends (hunter_a, hunter_b) VALUES (?1, ?2) ON CONFLICT DO NOTHING",
            a,
            b
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(found)
}

/// Ends a friendship, for both.
pub async fn unfriend(db: &SqlitePool, one: i64, other: i64) -> Result<()> {
    let (a, b) = (one.min(other), one.max(other));
    sqlx::query!(
        "DELETE FROM friends WHERE hunter_a = ?1 AND hunter_b = ?2",
        a,
        b
    )
    .execute(db)
    .await?;
    Ok(())
}

/// A hunter's friends (Capcom ID and name), in the order they became friends.
pub async fn friends(db: &SqlitePool, hunter: i64) -> Result<Vec<(String, Vec<u8>)>> {
    Ok(sqlx::query!(
        r#"SELECT h.capcom_id, h.name FROM friends f
           JOIN hunters h ON h.id = CASE WHEN f.hunter_a = ?1 THEN f.hunter_b ELSE f.hunter_a END
           WHERE f.hunter_a = ?1 OR f.hunter_b = ?1 ORDER BY f.created_at, h.id"#,
        hunter
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(|r| (r.capcom_id, r.name))
    .collect())
}

/// Marks the request `from` sent `to` delivered to the recipient's console.
pub async fn delivered(db: &SqlitePool, from: i64, to: i64) -> Result<()> {
    sqlx::query!(
        "UPDATE friend_requests SET delivered = 1 WHERE from_hunter = ?1 AND to_hunter = ?2",
        from,
        to
    )
    .execute(db)
    .await?;
    Ok(())
}

/// The friend requests a hunter's console hasn't received yet (sender's Capcom ID and name, message), now marked
/// delivered.
pub async fn undelivered_requests(
    db: &SqlitePool,
    hunter: i64,
) -> Result<Vec<(String, Vec<u8>, Vec<u8>)>> {
    Ok(sqlx::query!(
        r#"UPDATE friend_requests SET delivered = 1 WHERE to_hunter = ?1 AND delivered = 0
           RETURNING (SELECT capcom_id FROM hunters WHERE id = from_hunter) AS "capcom_id!: String",
                     (SELECT name FROM hunters WHERE id = from_hunter) AS "name!: Vec<u8>", message"#,
        hunter
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(|r| (r.capcom_id, r.name, r.message))
    .collect())
}

/// Stores a hunter's profile: the raw block and its known fields.
pub async fn save_profile(db: &SqlitePool, hunter: i64, profile: &Profile) -> Result<()> {
    let raw = &profile.raw()[..];
    sqlx::query!(
        r#"INSERT INTO hunter_profiles (hunter_id, raw, hr, weapon_type) VALUES (?1, ?2, ?3, ?4)
           ON CONFLICT (hunter_id) DO UPDATE
           SET raw = excluded.raw, hr = excluded.hr, weapon_type = excluded.weapon_type,
               updated_at = CURRENT_TIMESTAMP"#,
        hunter,
        raw,
        profile.hr,
        profile.weapon
    )
    .execute(db)
    .await?;
    Ok(())
}

pub async fn block(db: &SqlitePool, blocker: i64, blocked: i64) -> Result<()> {
    sqlx::query!(
        "INSERT INTO blacklist (blocker, blocked) VALUES (?1, ?2) ON CONFLICT DO NOTHING",
        blocker,
        blocked
    )
    .execute(db)
    .await?;
    Ok(())
}

pub async fn unblock(db: &SqlitePool, blocker: i64, blocked: i64) -> Result<()> {
    sqlx::query!(
        "DELETE FROM blacklist WHERE blocker = ?1 AND blocked = ?2",
        blocker,
        blocked
    )
    .execute(db)
    .await?;
    Ok(())
}

/// Whether `blocker` blocked `blocked`.
pub async fn blocks(db: &SqlitePool, blocker: i64, blocked: i64) -> Result<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT 1 AS "x!: i64" FROM blacklist WHERE blocker = ?1 AND blocked = ?2"#,
        blocker,
        blocked
    )
    .fetch_optional(db)
    .await?
    .is_some())
}

/// A hunter's black list (Capcom ID and name), in the order they were added.
pub async fn blacklist(db: &SqlitePool, blocker: i64) -> Result<Vec<(String, Vec<u8>)>> {
    Ok(sqlx::query!(
        r#"SELECT h.capcom_id, h.name FROM blacklist b JOIN hunters h ON h.id = b.blocked
           WHERE b.blocker = ?1 ORDER BY b.created_at, h.id"#,
        blocker
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(|r| (r.capcom_id, r.name))
    .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_use_the_game_alphabet() {
        let id = random_id(SUPPORT_CODE_LEN);
        assert_eq!(id.len(), SUPPORT_CODE_LEN);
        assert!(
            id.bytes()
                .all(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
        );
        assert_ne!(random_id(SUPPORT_CODE_LEN), id);
    }

    #[test]
    fn the_hmac_depends_on_the_secret() {
        let (a, b) = (Secret(b"a".to_vec()), Secret(b"b".to_vec()));
        assert_eq!(hmac(&a, "CODE").len(), 32);
        assert_eq!(hmac(&a, "CODE"), hmac(&a, "CODE"));
        assert_ne!(hmac(&a, "CODE"), hmac(&b, "CODE"));
    }
}
