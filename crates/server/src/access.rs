//! Bans (`access_rules`, the wiki's "Project", the data model): an admin bans a player's console, all its hunters with
//! it, and the Wii it last logged in from (`devices`, ADR 0013), so a new save doesn't lift it. A ban is checked
//! when the console logs in (the Wii at OPN, the console at `game::lmp`) and lifted by revoking it. The table also
//! holds the whitelist (kind `allow`, on a Wii) and the ops (kind `op`, on a hunter); their commands are a later
//! sprint (the Roadmap: "Moderation, Minecraft-style").

use anyhow::Result;
use sqlx::SqlitePool;
use uuid::Uuid;

/// Bans the console of the hunter with this Capcom ID and its Wii, as the hunter `by` (a Capcom ID: the admin who
/// typed `/ban`): `false` if there's no such hunter.
pub async fn ban(db: &SqlitePool, capcom_id: &str, reason: &str, by: &str) -> Result<bool> {
    let public_id = Uuid::now_v7();
    let public_id = public_id.as_bytes().as_slice();
    let device_rule = Uuid::now_v7();
    let device_rule = device_rule.as_bytes().as_slice();
    let mut tx = db.begin().await?;
    let done = sqlx::query!(
        "INSERT INTO access_rules (public_id, kind_id, console_id, reason, created_by)
         SELECT ?1, rule_kinds.id, hunters.console_id, ?3, (SELECT id FROM hunters WHERE capcom_id = ?4)
         FROM hunters, rule_kinds
         WHERE hunters.capcom_id = ?2 AND rule_kinds.name = 'ban'",
        public_id,
        capcom_id,
        reason,
        by
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO access_rules (public_id, kind_id, device_id, reason, created_by)
         SELECT ?1, rule_kinds.id, consoles.device_id, ?3, (SELECT id FROM hunters WHERE capcom_id = ?4)
         FROM hunters JOIN consoles ON consoles.id = hunters.console_id, rule_kinds
         WHERE hunters.capcom_id = ?2 AND consoles.device_id IS NOT NULL AND rule_kinds.name = 'ban'",
        device_rule,
        capcom_id,
        reason,
        by
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(done.rows_affected() == 1)
}

/// Lifts the bans on the console of the hunter with this Capcom ID and on its Wii: `false` if it had none.
pub async fn unban(db: &SqlitePool, capcom_id: &str, reason: &str) -> Result<bool> {
    let done = sqlx::query!(
        "UPDATE access_rules SET revoked_at = CURRENT_TIMESTAMP, revocation_reason = ?2
         WHERE revoked_at IS NULL
           AND kind_id = (SELECT id FROM rule_kinds WHERE name = 'ban')
           AND (console_id = (SELECT console_id FROM hunters WHERE capcom_id = ?1)
                OR device_id = (SELECT consoles.device_id FROM hunters JOIN consoles ON consoles.id = hunters.console_id
                                WHERE hunters.capcom_id = ?1))",
        capcom_id,
        reason
    )
    .execute(db)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// At a hop (a ticket's next connection, LMP or FMP): whether a console banned since its login must be dropped. A
/// database failure lets it through (ADR 0009: a failing database keeps players; its next fresh login
/// checks again).
pub async fn banned_since(shared: &crate::Shared, console_id: i64) -> bool {
    let Some(db) = shared.db.pool() else {
        return false;
    };
    match banned(db, console_id).await {
        Ok(banned) => banned,
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "database failed, ban not checked at a hop");
            false
        }
    }
}

/// Whether a console, or the Wii it last logged in from, is banned now.
pub async fn banned(db: &SqlitePool, console_id: i64) -> Result<bool> {
    let row = sqlx::query_scalar!(
        r#"SELECT EXISTS (
             SELECT 1 FROM access_rules JOIN rule_kinds ON rule_kinds.id = access_rules.kind_id
             WHERE rule_kinds.name = 'ban' AND revoked_at IS NULL
               AND (expires_at IS NULL OR expires_at > CURRENT_TIMESTAMP)
               AND (console_id = ?1 OR device_id = (SELECT device_id FROM consoles WHERE id = ?1))
           ) AS "banned!: bool""#,
        console_id
    )
    .fetch_one(db)
    .await?;
    Ok(row)
}

/// Whether a Wii, known by its MAC and serial's HMAC (`nas.rs`), is banned now.
pub async fn device_banned(db: &SqlitePool, device: &[u8]) -> Result<bool> {
    let row = sqlx::query_scalar!(
        r#"SELECT EXISTS (
             SELECT 1 FROM access_rules JOIN rule_kinds ON rule_kinds.id = access_rules.kind_id
             JOIN devices ON devices.id = access_rules.device_id
             WHERE rule_kinds.name = 'ban' AND revoked_at IS NULL
               AND (expires_at IS NULL OR expires_at > CURRENT_TIMESTAMP)
               AND devices.mac_serial_hmac = ?1
           ) AS "banned!: bool""#,
        device
    )
    .fetch_one(db)
    .await?;
    Ok(row)
}
