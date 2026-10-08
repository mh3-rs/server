# M1 — Database schema (draft for review)

**Status: draft, to challenge.** Becomes `migrations/0001_schema.sql` once settled (rewritten in place until
1.0.0, see the schema policy in `M1 login funnel.md`).

Inputs: decisions D1–D7, V1–V4, support code storage, identity (`M1 login funnel.md`).

## Principles

- **PostgreSQL 18.**
- **Keys**: every table has an internal `id bigint GENERATED ALWAYS AS IDENTITY` primary key, used by foreign keys.
  Entities that people reference (website URLs, API, logs) also have a `public_id uuid NOT NULL UNIQUE DEFAULT
  uuidv7()`: **no internal key ever leaves the database**. Exceptions: `settings` (keyed by name) and the audit log
  (UUID v7 primary key, append-only). UUIDs stop enumeration; authorization checks on every request still decide
  who may see or change what.
- **No enum types**: enumerations are `text` with a `CHECK`.
- **No history**: rows are overwritten in place (terms excepted: each version is a row, see `terms`).
- **Defaults live in the code, the database only holds overrides**: every text key (an enum) and the default terms
  have an English default in the code, so a missing text is a compile error. Nothing is inserted at startup or
  seeded. Lookup: database (key, console language) → database (key, `en`) → code default.
- **Only permanent data.** Sessions, tickets, pairing codes, presence, populations, ownership leases and the fleet
  registry are in the cache (V1). Live worlds (gates, cities, quests in progress) are in node memory.
- **No secrets in clear**: support codes are stored as HMAC-SHA256(pepper, code), 32 bytes; the pepper comes from
  the environment and never changes.
- **Game bytes stay bytes**: hunter names are `bytea` (the game may use Shift-JIS); identifiers the game formats
  (`0-9A-Z`) are `text` with a `CHECK`.
- `timestamptz` everywhere; `created_at` defaults to `now()`.
- Names: plural tables, `snake_case`, foreign keys `<entity>_id`.

## M1 tables

```sql
-- Key/value settings, readable by every node; editable by admins (website, M7). The server knows every key
-- (a list in the code). Known keys:
--   'natneg_hostname'  the only accepted NAT negotiation hostname (D3)
--   'registration'     'open' | 'closed'                        (website accounts, M7)
--   'whitelist'        'off' | 'on_register' | 'admin'           (M5: linking a console to an account whitelists
--                      it, or linking then an admin's approval)
CREATE TABLE settings (
    key         varchar(32) PRIMARY KEY,
    value       text        NOT NULL,
    updated_at  timestamptz NOT NULL DEFAULT now()
);

-- Overrides of the texts the server sends (D1, D6) except the terms: announce, no-charge, refusal and kick
-- messages, vulgarity lists, server type names and descriptions. Empty by default: every key (an enum in the code)
-- has an English default in the code. Lookup: (key, console language) → (key, 'en') → code default.
CREATE TABLE texts (
    id          bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    key         varchar(64) NOT NULL,          -- e.g. 'announce', 'refusal.not_ready', 'server_type.rookie.name'
    language    varchar(8)  NOT NULL,          -- ISO 639-1 ('en', 'fr', 'de', …), mapped from the game's code
    value       text        NOT NULL,          -- in the game's markup (<BR>, <CENTER>, <SIZE=n>, <C=n>, <END>)
    updated_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (key, language)
);

-- Overrides of the terms and sub-terms. Each edit inserts a new row; its `id` is the version the console
-- compares (AnsTermsVersion): it only ever grows, so the console re-shows edited terms whether it checks for a
-- different or a higher number. The built-in English terms (in the code) are version 1, so the identity starts at
-- 2. Current terms: latest row for (kind, console language) → latest for (kind, 'en') → built-in.
CREATE TABLE terms (
    id          bigint      GENERATED ALWAYS AS IDENTITY (START WITH 2) PRIMARY KEY
                            CHECK (id <= 4294967295),     -- u32 on the wire
    kind        text        NOT NULL CHECK (kind IN ('terms', 'subterms')),
    language    varchar(8)  NOT NULL,
    value       text        NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX terms_current ON terms (kind, language, id DESC);

-- The four server types of the game (fixed by the game's content), with their HR limits.
CREATE TABLE server_types (
    id          bigint   GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    type        text     NOT NULL UNIQUE CHECK (type IN ('open', 'rookie', 'expert', 'recruiting')),
    hr_min      smallint NOT NULL CHECK (hr_min BETWEEN 0 AND 999),
    hr_max      smallint NOT NULL CHECK (hr_max BETWEEN hr_min AND 999)
);

-- World servers shown in the server list (D2). Gates and cities are generated from the counts.
-- The game picks a server by its index in the list it was just sent, which the server maps back to the row with
-- that same list; `position` is only the display order, so reordering only changes what players see.
CREATE TABLE world_servers (
    id              bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    public_id       uuid        NOT NULL UNIQUE DEFAULT uuidv7(),   -- leases, presence, logs, website
    position        smallint    NOT NULL UNIQUE,
    name            text        NOT NULL UNIQUE,   -- e.g. 'Valor1'; max length to check against FmpListData
    server_type_id  bigint      NOT NULL REFERENCES server_types,
    capacity        integer     NOT NULL DEFAULT 2000 CHECK (capacity > 0),
    gate_count      smallint    NOT NULL DEFAULT 40 CHECK (gate_count BETWEEN 1 AND 40),
    gate_capacity   integer     NOT NULL DEFAULT 100 CHECK (gate_capacity > 0),
    city_count      smallint    NOT NULL DEFAULT 40 CHECK (city_count BETWEEN 1 AND 40),
    enabled         boolean     NOT NULL DEFAULT true,
    created_at      timestamptz NOT NULL DEFAULT now()
);

-- A console, identified by its online support code (never stored in clear).
CREATE TABLE consoles (
    id                  bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    public_id           uuid        NOT NULL UNIQUE DEFAULT uuidv7(),   -- logs ('console' span field), website
    support_code_hmac   bytea       NOT NULL UNIQUE CHECK (length(support_code_hmac) = 32),
    region              text        NOT NULL CHECK (region IN ('RMHP08', 'RMHE08')),  -- disc id (D3)
    created_at          timestamptz NOT NULL DEFAULT now(),
    last_seen_at        timestamptz NOT NULL DEFAULT now()
);

-- One of the console's 6 Capcom ID slots: an online identity that a hunter of the save plays under.
-- The game never renames a hunter, so a different name on a slot means another hunter took it: name and profile
-- are overwritten, friends, blacklist and pending friend requests are dropped (a friendship is with a person),
-- an `info` line is logged. The Capcom ID stays, with everything moderation attaches to it (anti-cheat flags,
-- bans): a fresh Capcom ID on replacement would let a flagged player clean their record by switching hunters.
-- No history. To check in the game: whether a Capcom ID can be deleted (then: delete the row and its links).
CREATE TABLE hunters (
    id              bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    public_id       uuid        NOT NULL UNIQUE DEFAULT uuidv7(),   -- website, API
    console_id      bigint      NOT NULL REFERENCES consoles ON DELETE CASCADE,
    slot            smallint    NOT NULL CHECK (slot BETWEEN 1 AND 6),
    capcom_id       text        NOT NULL UNIQUE CHECK (capcom_id ~ '^[0-9A-Z]{6}$'),
    name            bytea       NOT NULL,           -- as the game sends it
    created_at      timestamptz NOT NULL DEFAULT now(),
    last_seen_at    timestamptz NOT NULL DEFAULT now(),
    UNIQUE (console_id, slot)
);
```

Seed data in the SQL file (`INSERT … ON CONFLICT DO NOTHING`): reference data only, no texts or terms (their
defaults are in the code):
- `server_types`: open 0–999, rookie 0–30, expert 31–999, recruiting 0–999 (MH3SP's values).
- `world_servers`: Valor1–4 (open), Beginners1–2 (rookie), Veterans1–2 (expert), Greed1–4 (recruiting), with their
  `public_id`s written in the seed (not generated) so every node and every reset agrees.
- `settings`: `registration` = `closed`, `whitelist` = `off`; no default `natneg_hostname`: it is derived from the
  deployment's domain (our patch, D3), so a fresh install without it refuses every disc with a clear "server not
  configured" log line.

## Later tables, sketched to check the M1 tables hold

Not part of M1: shown only to see whether M1's tables would need reshaping later. They don't, as drafted: every
later table points at `consoles` or `hunters` by id.

```sql
-- M5/M7: website accounts and the whitelist.
accounts        (id, public_id, email unique, password_hash /* argon2: human passwords */, created_at, …)
consoles        + account_id bigint NULL REFERENCES accounts ON DELETE SET NULL  -- linked by pairing
                + access text CHECK (access IN ('pending', 'approved', 'denied'))  -- per the 'whitelist' setting:
                  'on_register' approves on linking, 'admin' waits for an admin
bans            (id, console_id NULL, hunter_id NULL, reason, banned_by, created_at, expires_at NULL)
-- Ban evasion: a console's identity is its support code, kept in the save's network file; deleting the file
-- gives a new identity. Mitigations: the whitelist with accounts (a banned account can't pair new consoles) and,
-- with our own Nintendo login, bans tied to hardware identifiers (Wii serial, MAC) seen by it.

-- M3 (in memory from M2): the hunter's public profile, the 256-byte block the console writes with
-- ReqUserBinarySet and the server hands to other players (AnsLayerUserList, NtcLayerIn,
-- NtcUserBinaryNotice). Self-reported by the console: the save is the truth. Known: HR (u16 @0),
-- weapon type (u8 @5); likely equipment and appearance (other consoles draw the hunter from it).
-- Persisted for the website (hunter pages) and anti-cheat (impossible values or jumps, M6).
-- History (one row per change) only if M6 needs it.
-- Only parsed, validated fields are stored, never the raw 256 bytes (they stay in memory while the player is
-- connected, for forwarding, until the block can be rebuilt from fields; see M3 in the global roadmap).
hunter_profiles (hunter_id PRIMARY KEY, hr smallint, weapon_type smallint, … /* columns added as fields get mapped */, updated_at)

-- M3: social (friends also managed from the website, M7: the game fetches its list from the server).
friends         (hunter_id, friend_id, created_at, PRIMARY KEY (hunter_id, friend_id))
friend_requests (from_hunter_id, to_hunter_id, message bytea, created_at)
blacklist       (hunter_id, blocked_hunter_id, created_at)

-- M2: event content.
event_quests    (id, name, definition jsonb, …)
quest_rotation  (id, quest_id, starts_at, ends_at, …)   -- Jhen Mohran windows included
trading_post    (…)

-- M6/M7: anti-cheat and administration.
anticheat_flags (id, hunter_id, kind, evidence jsonb, created_at, reviewed_by NULL, …)
admins          (id, account_id, role, …)
admin_actions   (id uuid DEFAULT uuidv7() PRIMARY KEY /* audit log: append-only */, admin_id, action, target, details jsonb, created_at)
```

## Answered

1. A Capcom ID slot is an online identity. Another hunter taking it (a new name) overwrites name and profile and
   drops its social links; the Capcom ID and its moderation records stay, so switching hunters can't clean a
   record. (To check: whether the game can delete a Capcom ID.)
2. `last_seen_at`: kept (consoles and hunters).
3. Region: on the console.
4. Texts and terms: the code holds every default (keys are an enum), the database only overrides
   (lookup: language → `en` → code). Terms in their own table, the identity key being the version (built-in = 1,
   overrides from 2).
5. World servers: `position` is only the display order; rows are referenced internally by `id`, outside by
   `public_id`.
6. Whitelist: instance settings `registration` (open/closed) and `whitelist` (off / on_register / admin).
7. Enumerations: `text` with a `CHECK`.
8. PostgreSQL 18, `uuidv7()` for public ids (dev compose and CI on `postgres:18`).
9. Keys: internal `bigint` identity everywhere (except `settings` and the audit log), `public_id` UUID on what
   people reference; internal keys never leave the database.

## Next

A second review pass on the whole file, then it becomes `migrations/0001_schema.sql`.
