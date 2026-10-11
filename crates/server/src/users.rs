//! Who signs in to the API (`api.rs`), and their tokens (`../docs/schema.md`, S2–S3).
//!
//! A token reads `mh3sp_<public>.<secret>`: the key `mh3sp_<public>` names it in lists, and only the SHA-256 of the
//! whole token is stored (a high-entropy secret needs no slow hash). The `mh3sp_` prefix lets secret scanners spot a
//! leaked one.

use anyhow::{Context, Result, bail};
use sqlx::SqlitePool;
use uuid::Uuid;

/// What a user may do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Player,
    Admin,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Role::Player => "player",
            Role::Admin => "admin",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        [Role::Player, Role::Admin]
            .into_iter()
            .find(|r| r.name() == name)
    }
}

/// A signed-in user, as the API sees them.
#[derive(Clone, Debug)]
pub struct User {
    pub public_id: Uuid,
    pub role: Role,
}

/// A new user (no password: tokens only, until the website's sign-in): their public id.
pub async fn add(db: &SqlitePool, email: &str, role: Role) -> Result<Uuid> {
    let email = email.trim().to_lowercase();
    if !email.contains('@') {
        bail!("{email:?} is not an email address");
    }
    let public_id = Uuid::now_v7();
    let bytes = public_id.as_bytes().as_slice();
    let role = role.name();
    sqlx::query!(
        "INSERT INTO users (public_id, email, role_id) SELECT ?1, ?2, id FROM roles WHERE name = ?3",
        bytes,
        email,
        role
    )
    .execute(db)
    .await
    .with_context(|| format!("add {email}"))?;
    Ok(public_id)
}

/// A new token for the user with this email: the token itself, shown once and never stored.
pub async fn issue(db: &SqlitePool, email: &str, usage: &str) -> Result<String> {
    let email = email.trim().to_lowercase();
    let mut public = [0u8; 8];
    let mut secret = [0u8; 32];
    openssl::rand::rand_bytes(&mut public).context("random source")?;
    openssl::rand::rand_bytes(&mut secret).context("random source")?;
    let key = format!("mh3sp_{}", hex::encode(public));
    let token = format!("{key}.{}", hex::encode(secret));
    let hash = openssl::sha::sha256(token.as_bytes());
    let hash = hash.as_slice();
    let inserted = sqlx::query!(
        "INSERT INTO tokens (key, hash, user_id, usage_id)
         SELECT ?1, ?2, users.id, usages.id FROM users, usages WHERE users.email = ?3 AND usages.name = ?4",
        key,
        hash,
        email,
        usage
    )
    .execute(db)
    .await?;
    if inserted.rows_affected() == 0 {
        bail!("no user {email}, or no usage {usage}");
    }
    Ok(token)
}

/// Sets a user's password (argon2): `false` if there's no such user.
pub async fn set_password(db: &SqlitePool, email: &str, password: &str) -> Result<bool> {
    if password.chars().count() < 12 {
        bail!("a password of 12 characters at least");
    }
    let email = email.trim().to_lowercase();
    let password = password.to_owned();
    let hash = tokio::task::spawn_blocking(move || {
        use argon2::password_hash::PasswordHasher;
        argon2::Argon2::default()
            .hash_password(password.as_bytes())
            .map(|h| h.to_string())
    })
    .await?
    .map_err(|e| anyhow::anyhow!("argon2: {e}"))?;
    let done = sqlx::query!(
        "UPDATE users SET password_hash = ?2 WHERE email = ?1",
        email,
        hash
    )
    .execute(db)
    .await?;
    Ok(done.rows_affected() == 1)
}

// An argon2 hash of a random password: what an unknown email is checked against.
const NOBODY: &str = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$Y5rr4uY0hRl3JX7AqQrMgwqhcP0vg0n+2c3Tj7jVQWs";

/// Signs a user in by email and password: a new `session` token, or `None`. An unknown email costs the same check
/// as a wrong password (no telling which emails exist).
pub async fn sign_in(db: &SqlitePool, email: &str, password: &str) -> Result<Option<String>> {
    let email = email.trim().to_lowercase();
    let stored = sqlx::query_scalar!("SELECT password_hash FROM users WHERE email = ?1", email)
        .fetch_optional(db)
        .await?
        .flatten();
    let known = stored.is_some();
    let hash = stored.unwrap_or_else(|| NOBODY.to_owned());
    let password = password.to_owned();
    let matches = tokio::task::spawn_blocking(move || {
        use argon2::password_hash::PasswordVerifier;
        argon2::password_hash::phc::PasswordHash::new(&hash).is_ok_and(|h| {
            argon2::Argon2::default()
                .verify_password(password.as_bytes(), &h)
                .is_ok()
        })
    })
    .await?;
    match known && matches {
        true => Ok(Some(issue(db, &email, "session").await?)),
        false => Ok(None),
    }
}

/// Revokes the token itself (signing out): `false` if it wasn't valid.
pub async fn sign_out(db: &SqlitePool, token: &str) -> Result<bool> {
    let hash = openssl::sha::sha256(token.as_bytes());
    let hash = hash.as_slice();
    let done = sqlx::query!(
        "UPDATE tokens SET revoked_at = CURRENT_TIMESTAMP, revocation_reason = 'signed out'
         WHERE hash = ?1 AND revoked_at IS NULL",
        hash
    )
    .execute(db)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// The user a token belongs to, if it is valid: known, not revoked, not expired.
pub async fn authenticate(db: &SqlitePool, token: &str) -> Result<Option<User>> {
    let hash = openssl::sha::sha256(token.as_bytes());
    let hash = hash.as_slice();
    let row = sqlx::query!(
        r#"SELECT users.public_id, roles.name AS role FROM tokens
           JOIN users ON users.id = tokens.user_id
           JOIN roles ON roles.id = users.role_id
           JOIN usages ON usages.id = tokens.usage_id
           WHERE tokens.hash = ?1 AND tokens.revoked_at IS NULL
             AND (usages.lifetime_seconds IS NULL
                  OR unixepoch(tokens.created_at) + usages.lifetime_seconds > unixepoch())"#,
        hash
    )
    .fetch_optional(db)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(User {
        public_id: Uuid::from_slice(&row.public_id)?,
        role: Role::parse(&row.role).with_context(|| format!("role {:?}", row.role))?,
    }))
}

/// Revokes a token by its key: `false` if there's no such token still valid.
pub async fn revoke(db: &SqlitePool, key: &str, reason: &str) -> Result<bool> {
    let done = sqlx::query!(
        "UPDATE tokens SET revoked_at = CURRENT_TIMESTAMP, revocation_reason = ?2
         WHERE key = ?1 AND revoked_at IS NULL",
        key,
        reason
    )
    .execute(db)
    .await?;
    Ok(done.rows_affected() == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tokens_authenticate_until_revoked_or_expired() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("t.db").display());
        let db = crate::database::open(&url).await.unwrap();
        let id = add(&db, " Admin@Example.org ", Role::Admin).await.unwrap();
        assert!(
            add(&db, "admin@example.org", Role::Player).await.is_err(),
            "twice"
        );
        assert!(add(&db, "nobody", Role::Player).await.is_err());

        let token = issue(&db, "ADMIN@example.org", "cli").await.unwrap();
        let user = authenticate(&db, &token).await.unwrap().unwrap();
        assert_eq!((user.public_id, user.role), (id, Role::Admin));
        assert!(
            authenticate(&db, &format!("{token}x"))
                .await
                .unwrap()
                .is_none()
        );
        assert!(issue(&db, "other@example.org", "cli").await.is_err());
        assert!(issue(&db, "admin@example.org", "forever").await.is_err());

        let (key, _) = token.split_once('.').unwrap();
        assert!(revoke(&db, key, "test").await.unwrap());
        assert!(!revoke(&db, key, "again").await.unwrap());
        assert!(
            authenticate(&db, &token).await.unwrap().is_none(),
            "revoked"
        );

        // A session a month old has expired.
        let session = issue(&db, "admin@example.org", "session").await.unwrap();
        assert!(authenticate(&db, &session).await.unwrap().is_some());
        sqlx::query("UPDATE tokens SET created_at = datetime('now', '-31 days')")
            .execute(&db)
            .await
            .unwrap();
        assert!(
            authenticate(&db, &session).await.unwrap().is_none(),
            "expired"
        );
    }

    #[tokio::test]
    async fn passwords_sign_in_and_out() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("t.db").display());
        let db = crate::database::open(&url).await.unwrap();
        add(&db, "a@example.org", Role::Player).await.unwrap();
        // What an unknown email is checked against must parse, or it would answer at once (telling it apart).
        assert!(argon2::password_hash::phc::PasswordHash::new(NOBODY).is_ok());
        // No password yet: no sign-in, whatever is typed.
        assert!(sign_in(&db, "a@example.org", "").await.unwrap().is_none());
        assert!(set_password(&db, "a@example.org", "short").await.is_err());
        assert!(
            !set_password(&db, "b@example.org", "long enough pass")
                .await
                .unwrap()
        );
        assert!(
            set_password(&db, "A@example.org", "long enough pass")
                .await
                .unwrap()
        );
        assert!(
            sign_in(&db, "a@example.org", "long enough pasS")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            sign_in(&db, "nobody@example.org", "long enough pass")
                .await
                .unwrap()
                .is_none()
        );
        let token = sign_in(&db, " A@Example.org", "long enough pass")
            .await
            .unwrap()
            .unwrap();
        assert!(authenticate(&db, &token).await.unwrap().is_some());
        assert!(sign_out(&db, &token).await.unwrap());
        assert!(
            authenticate(&db, &token).await.unwrap().is_none(),
            "signed out"
        );
        assert!(!sign_out(&db, &token).await.unwrap());
    }
}
