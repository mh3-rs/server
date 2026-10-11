//! Bans (`access_rules`, `../docs/schema.md` S5): an admin bans a player's console, all its hunters with it; a ban is
//! checked when the console logs in (`game::lmp`) and lifted by revoking it. The whitelist (rules of kind `allow`)
//! is a question still (`roadmap/questions.md`).

use anyhow::Result;
use sqlx::SqlitePool;
use uuid::Uuid;

/// Bans the console of the hunter with this Capcom ID, by the user with public id `by` (`None`: the operator):
/// `false` if there's no such hunter.
pub async fn ban(db: &SqlitePool, capcom_id: &str, reason: &str, by: Option<Uuid>) -> Result<bool> {
    let public_id = Uuid::now_v7();
    let public_id = public_id.as_bytes().as_slice();
    let by = by.map(|u| u.as_bytes().to_vec());
    let done = sqlx::query!(
        "INSERT INTO access_rules (public_id, kind_id, console_id, reason, created_by)
         SELECT ?1, rule_kinds.id, hunters.console_id, ?3, (SELECT id FROM users WHERE public_id = ?4)
         FROM hunters, rule_kinds
         WHERE hunters.capcom_id = ?2 AND rule_kinds.name = 'ban'",
        public_id,
        capcom_id,
        reason,
        by
    )
    .execute(db)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Lifts the bans on the console of the hunter with this Capcom ID: `false` if it had none.
pub async fn unban(db: &SqlitePool, capcom_id: &str, reason: &str) -> Result<bool> {
    let done = sqlx::query!(
        "UPDATE access_rules SET revoked_at = CURRENT_TIMESTAMP, revocation_reason = ?2
         WHERE revoked_at IS NULL
           AND kind_id = (SELECT id FROM rule_kinds WHERE name = 'ban')
           AND console_id = (SELECT console_id FROM hunters WHERE capcom_id = ?1)",
        capcom_id,
        reason
    )
    .execute(db)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// At a hop (a ticket's next connection, LMP or FMP): whether a console banned since its login must be dropped. A
/// database failure lets it through (architecture decision 9: a failing database keeps players; its next fresh login
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

/// The bans in force, the latest first: public id, the console's Capcom IDs (comma-separated, sorted), reason, since.
pub async fn bans(db: &SqlitePool) -> Result<Vec<(Uuid, String, String, String)>> {
    let rows = sqlx::query!(
        r#"SELECT r.public_id,
                  (SELECT group_concat(capcom_id, ',' ORDER BY capcom_id) FROM hunters
                   WHERE console_id = r.console_id) AS "ids: String",
                  r.reason, r.created_at
           FROM access_rules r JOIN rule_kinds k ON k.id = r.kind_id
           WHERE k.name = 'ban' AND r.console_id IS NOT NULL AND r.revoked_at IS NULL
             AND (r.expires_at IS NULL OR r.expires_at > CURRENT_TIMESTAMP)
           ORDER BY r.id DESC"#
    )
    .fetch_all(db)
    .await?;
    rows.into_iter()
        .map(|r| {
            let id = Uuid::from_slice(&r.public_id)?;
            Ok((id, r.ids.unwrap_or_default(), r.reason, r.created_at))
        })
        .collect()
}

/// Whether a console is banned now.
pub async fn banned(db: &SqlitePool, console_id: i64) -> Result<bool> {
    let row = sqlx::query_scalar!(
        r#"SELECT EXISTS (
             SELECT 1 FROM access_rules JOIN rule_kinds ON rule_kinds.id = access_rules.kind_id
             WHERE rule_kinds.name = 'ban' AND console_id = ?1 AND revoked_at IS NULL
               AND (expires_at IS NULL OR expires_at > CURRENT_TIMESTAMP)
           ) AS "banned!: bool""#,
        console_id
    )
    .fetch_one(db)
    .await?;
    Ok(row)
}
