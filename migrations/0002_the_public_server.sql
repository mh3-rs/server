-- The public server has no users, tokens or API: it knows consoles, hunters and Wii, and nothing else. A forward
-- migration, never a reset: the published image's databases hold players' Capcom IDs, friends and bans. 0001 is never
-- edited again (its checksum is checked by sqlx, and a test pins it).
--
-- `access_rules` takes its final shape, one row per grant: a `ban` on a console or on its Wii, an `allow` on a Wii (the
-- whitelist), an `op` on a hunter. `created_by` is the hunter who typed the command, NULL for the operator's own
-- commands; `reason` is optional (an `allow` or an `op` has none). Rebuilt because two of its columns pointed at
-- `users` (SQLite can't drop a column a constraint uses), so it is done once, here.
--
-- The rules that targeted a user are not copied (nothing ever wrote one), and a ban made through the API keeps its
-- row but loses its author: the users are gone.

CREATE TABLE access_rules_new (
    id                  INTEGER PRIMARY KEY,
    public_id           BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
    kind_id             INTEGER NOT NULL REFERENCES rule_kinds (id),
    console_id          INTEGER REFERENCES consoles (id),
    hunter_id           INTEGER REFERENCES hunters (id),
    device_id           INTEGER REFERENCES devices (id),
    reason              TEXT,
    -- The hunter who typed the command. NULL: the operator.
    created_by          INTEGER REFERENCES hunters (id),
    created_at          TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    -- NULL: until revoked.
    expires_at          TEXT,
    revoked_at          TEXT,
    revocation_reason   TEXT,
    CHECK ((console_id IS NOT NULL) + (hunter_id IS NOT NULL) + (device_id IS NOT NULL) = 1),
    CHECK ((revoked_at IS NULL) = (revocation_reason IS NULL))
) STRICT;

INSERT INTO access_rules_new (id, public_id, kind_id, console_id, hunter_id, device_id, reason,
                              created_at, expires_at, revoked_at, revocation_reason)
SELECT id, public_id, kind_id, console_id, hunter_id, device_id, reason,
       created_at, expires_at, revoked_at, revocation_reason
FROM access_rules WHERE user_id IS NULL;

DROP TABLE access_rules;
ALTER TABLE access_rules_new RENAME TO access_rules;
CREATE INDEX access_rules_console ON access_rules (console_id) WHERE console_id IS NOT NULL;
CREATE INDEX access_rules_device ON access_rules (device_id) WHERE device_id IS NOT NULL;
CREATE INDEX access_rules_hunter ON access_rules (hunter_id) WHERE hunter_id IS NOT NULL;

DROP TABLE tokens;
DROP TABLE users;
DROP TABLE usages;
DROP TABLE roles;

INSERT INTO rule_kinds (name) VALUES ('op');
