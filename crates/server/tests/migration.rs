//! The database a published server wrote (migration 0001 alone) is upgraded in place by the next release, and 0001 is
//! never edited again: the image is public, so no breaking change (the wiki's ADR on it, and `docs/CLAUDE.md`).

use std::str::FromStr;

use openssl::hash::{MessageDigest, hash};
use sqlx::SqlitePool;
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};

/// The SHA-256 of `migrations/0001_schema.sql` in the three published images (`7535f9bd`, `be746012`, `d6e1740e`).
/// Editing the file, a comment included, fails here instead of stopping an operator's server at its start (sqlx checks
/// the checksum of every migration a database already applied).
const MIGRATION_0001: &str = "57eb737f6636f2f1a30f9f1359e773ddfa72787b087844e9476b000ebc92e12b";

fn sha256(bytes: &[u8]) -> String {
    hex::encode(hash(MessageDigest::sha256(), bytes).unwrap())
}

#[test]
fn migration_0001_is_what_the_published_images_hold() {
    let file = include_bytes!("../../../migrations/0001_schema.sql");
    assert_eq!(sha256(file), MIGRATION_0001);
    // The check sees an edit as small as a space.
    let mut edited = file.to_vec();
    edited.push(b' ');
    assert_ne!(sha256(&edited), MIGRATION_0001);
}

/// A database as the published server left it: the options of `database::open`, and migration 0001 alone, applied by
/// sqlx's own migrator over a folder that holds only that file (so the migrations table is what that server wrote).
async fn published(dir: &std::path::Path) -> (SqlitePool, String) {
    let url = format!("sqlite://{}", dir.join("published.db").display());
    let options = SqliteConnectOptions::from_str(&url)
        .unwrap()
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);
    let pool = SqlitePool::connect_with(options).await.unwrap();
    let only_0001 = dir.join("only_0001");
    std::fs::create_dir(&only_0001).unwrap();
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../migrations/0001_schema.sql"
        ),
        only_0001.join("0001_schema.sql"),
    )
    .unwrap();
    Migrator::new(only_0001.as_path())
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();
    (pool, url)
}

/// A row in every table of 0001, the API's users included: two consoles on one Wii and a third alone, hunters with a
/// profile and equipment, a friendship, a message and a friend request, a block, a terms override, and four rules:
/// a ban on a console made by a user, a ban on a Wii, a lifted ban, and a rule that targets a user.
const ROWS: &str = "
INSERT INTO devices (id, public_id, mac_serial_hmac) VALUES (1, randomblob(16), randomblob(32));
INSERT INTO consoles (id, public_id, support_code_hmac, region, device_id)
    VALUES (1, randomblob(16), randomblob(32), 'RMHP08', 1), (2, randomblob(16), randomblob(32), 'RMHE08', NULL);
INSERT INTO hunters (id, public_id, console_id, slot, capcom_id, name) VALUES
    (1, randomblob(16), 1, 1, 'AAAAAA', x'4169'), (2, randomblob(16), 1, 2, 'BBBBBB', x'4262'),
    (3, randomblob(16), 2, 1, 'CCCCCC', x'43');
INSERT INTO hunter_profiles (hunter_id, hr, weapon_type, gender, voice, hair_colour, inner_colour, appearance_02,
    appearance_03, appearance_96, appearance_98, appearance_99, poogie_costume, title, status, comment, unknown_04,
    unknown_7c, unknown_81, unknown_92, unknown_93, unknown_9a, unknown_9b)
    VALUES (1, 51, 3, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, x'4869', 0, zeroblob(5), zeroblob(17), 0, 0, 0, 0);
INSERT INTO hunter_equipment (hunter_id, slot, kind, item, deco_count, deco_level, skill_1, skill_2, skill_3)
    VALUES (1, 0, 1, 10, 2, 3, 4, 5, 6);
INSERT INTO friends (hunter_a, hunter_b) VALUES (1, 3);
INSERT INTO messages (from_hunter, to_hunter, text, friend_request) VALUES (1, 3, x'486f', 0), (3, 2, x'4e', 1);
INSERT INTO blacklist (blocker, blocked) VALUES (3, 2);
INSERT INTO terms (kind, language, value) VALUES ('terms', 'en', 'Our terms.');
INSERT INTO users (id, public_id, email, role_id) VALUES (1, randomblob(16), 'admin@example.org',
    (SELECT id FROM roles WHERE name = 'admin'));
INSERT INTO tokens (key, hash, user_id, usage_id) VALUES ('mh3sp_abc', randomblob(32), 1, 1);
INSERT INTO access_rules (id, public_id, kind_id, console_id, reason, created_by)
    VALUES (1, randomblob(16), (SELECT id FROM rule_kinds WHERE name = 'ban'), 1, 'api ban', 1);
INSERT INTO access_rules (id, public_id, kind_id, device_id, reason)
    VALUES (2, randomblob(16), (SELECT id FROM rule_kinds WHERE name = 'ban'), 1, 'chat ban');
INSERT INTO access_rules (id, public_id, kind_id, console_id, reason, revoked_at, revocation_reason)
    VALUES (3, randomblob(16), (SELECT id FROM rule_kinds WHERE name = 'ban'), 2, 'old', CURRENT_TIMESTAMP, 'lifted');
INSERT INTO access_rules (id, public_id, kind_id, user_id, reason)
    VALUES (4, randomblob(16), (SELECT id FROM rule_kinds WHERE name = 'ban'), 1, 'on a user');
";

/// What each table of the game holds, as text, in one order: what an upgrade must leave as it was.
/// A rule as the tests read it: its id, kind, console, Wii, author, and whether it is lifted.
type Rule = (i64, i64, Option<i64>, Option<i64>, Option<i64>, bool);

const SNAPSHOTS: [&str; 9] = [
    "SELECT group_concat(hex(public_id) || hex(mac_serial_hmac) || created_at, '|') FROM devices",
    "SELECT group_concat(hex(public_id) || hex(support_code_hmac) || region || coalesce(device_id, '-'), '|') FROM consoles",
    "SELECT group_concat(hex(public_id) || console_id || '/' || slot || capcom_id || hex(name), '|') FROM hunters",
    "SELECT group_concat(hunter_id || '/' || hr || '/' || weapon_type || '/' || gender || hex(comment), '|') FROM hunter_profiles",
    "SELECT group_concat(hunter_id || '/' || slot || '/' || kind || '/' || item || '/' || deco_count, '|') FROM hunter_equipment",
    "SELECT group_concat(hunter_a || '-' || hunter_b, '|') FROM friends",
    "SELECT group_concat(coalesce(from_hunter, '-') || '>' || to_hunter || hex(text) || friend_request || sent, '|') FROM messages",
    "SELECT group_concat(blocker || '-' || blocked, '|') FROM blacklist",
    "SELECT group_concat(id || kind || language || value, '|') FROM terms",
];

async fn snapshots(pool: &SqlitePool) -> Vec<Option<String>> {
    let mut all = Vec::new();
    for sql in SNAPSHOTS {
        all.push(sqlx::query_scalar(sql).fetch_one(pool).await.unwrap());
    }
    all
}

async fn user_tables(pool: &SqlitePool) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name IN ('roles', 'users', 'usages', 'tokens')
         ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

/// 0002 keeps every console, hunter, friend, message, block and ban, drops the API's users, and leaves a table of rules
/// in its final shape.
#[tokio::test]
async fn a_published_database_is_upgraded_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let (pool, url) = published(dir.path()).await;
    sqlx::raw_sql(ROWS).execute(&pool).await.unwrap();
    // Before: what is to go is there, and so is everything that is to stay.
    assert_eq!(
        user_tables(&pool).await,
        ["roles", "tokens", "usages", "users"]
    );
    let before = snapshots(&pool).await;
    assert!(before.iter().all(Option::is_some), "a row in every table");
    pool.close().await;

    // The next release's own open: it runs the migrations the database lacks, 0002 only.
    let pool = server::database::open(&url).await.unwrap();
    assert_eq!(snapshots(&pool).await, before, "the game's rows");
    assert_eq!(user_tables(&pool).await, Vec::<String>::new());
    let rules: Vec<Rule> = sqlx::query_as(
        "SELECT id, kind_id, console_id, device_id, created_by, revoked_at IS NOT NULL FROM access_rules ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    // Both bans and the lifted one, their author gone with the user; the rule on a user wasn't copied.
    assert_eq!(
        rules,
        [
            (1, 1, Some(1), None, None, false),
            (2, 1, None, Some(1), None, false),
            (3, 1, Some(2), None, None, true),
        ]
    );
    let kinds: Vec<String> = sqlx::query_scalar("SELECT name FROM rule_kinds ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(kinds, ["ban", "allow", "op"]);
    let broken: Vec<(String,)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(broken.is_empty(), "{broken:?}");

    // The final shape: an op on a hunter and an allow on a Wii, each by a hunter; a rule with no target is refused.
    let grant = |sql: &'static str| sqlx::query(sql).execute(&pool);
    grant(
        "INSERT INTO access_rules (public_id, kind_id, hunter_id, created_by)
         VALUES (randomblob(16), (SELECT id FROM rule_kinds WHERE name = 'op'), 2, 1)",
    )
    .await
    .unwrap();
    grant(
        "INSERT INTO access_rules (public_id, kind_id, device_id)
         VALUES (randomblob(16), (SELECT id FROM rule_kinds WHERE name = 'allow'), 1)",
    )
    .await
    .unwrap();
    assert!(
        grant(
            "INSERT INTO access_rules (public_id, kind_id)
             VALUES (randomblob(16), (SELECT id FROM rule_kinds WHERE name = 'allow'))"
        )
        .await
        .is_err(),
        "a rule with no target"
    );
    pool.close().await;

    // Upgraded once: the next start finds nothing to do.
    let pool = server::database::open(&url).await.unwrap();
    assert_eq!(snapshots(&pool).await, before);
}
