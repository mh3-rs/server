# M1 — Database schema (draft for review)

**Status: under review.** The SQL lives in [`migrations/0001_schema.sql`](../migrations/0001_schema.sql), applied by
the server at startup and checked against PostgreSQL 18 (constraints exercised by hand: Capcom ID format, slot
range, HMAC length, disc ids, terms versions starting at 2, internal ids refused from outside). Rewritten in place
until 1.0.0 (`scripts/dev.sh --reset` after a change). This document keeps the reasoning; the file is the truth.

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

The SQL is in `migrations/0001_schema.sql`. Per table, what it holds and why:

| Table | One row per | Notes |
|---|---|---|
| `settings` | setting | key → value; `natneg_hostname` (D3), `access` (open / on_register / admin), `registration` (open/closed, website only). The list of keys and their values lives only in the code: the database checks the key format, the server validates meaning (on read, and through the admin API, the only writer). Seeded: `access = open`, `registration = closed`; `natneg_hostname` comes from the deployment's domain, and its absence refuses every disc with a clear log line |
| `texts` | override of a text × language | empty by default; keys are an enum in the code with English defaults (the database only checks their format); language is ISO 639-1 (the game's languages have no regional variants); lookup language → `en` → code |
| `terms` | edit of the terms or sub-terms × language | the identity key is the version sent to the console (u32): built-in terms are version 1, overrides start at 2, so edited terms are always re-shown |
| `consoles` | console | `support_code_hmac` (HMAC-SHA256 with the pepper, never the code), disc region, first/last seen |
| `hunters` | Capcom ID slot | an online identity a save hunter plays under; a new name on the slot means another hunter: name and profile overwritten, social links dropped, Capcom ID and moderation records kept; no history |

Not in the database: **world servers** (the 12 of MH3SP: names, types, capacity), **server types** (HR limits) and
the **world layout** (40 gates × 100 players, 40 cities × 4 per gate). They are game constants in the code,
optionally overridden by a `[[world_servers]]` list in `config.toml`, read at startup only (every node of a fleet
gets the same config). A world server is identified by its name (`Valor1`) in logs, leases and presence.

## Later tables, sketched to check the M1 tables hold

Not part of M1: shown only to see whether M1's tables would need reshaping later. They don't, as drafted: every
later table points at `consoles` or `hunters` by id.

```sql
-- M5/M7: website accounts and the whitelist.
accounts        (id, public_id, email unique, password_hash /* argon2: human passwords */, created_at, …)
consoles        + account_id bigint NULL REFERENCES accounts ON DELETE SET NULL  -- linked by pairing
                + approval text CHECK (approval IN ('pending', 'approved', 'denied'))  -- per the 'access' setting:
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
5. World servers: built-in constants (overridable at startup by `config.toml`), identified by name; not in the
   database. Same for server types and the world layout.
6. Access: setting `access` = `open` (no check) / `on_register` / `admin`; `registration` (open/closed) only
   matters with a website. The server never depends on the website (standalone: server + database, `access = open`).
7. Enumerations: `text` with a `CHECK`.
8. PostgreSQL 18, `uuidv7()` for public ids (dev compose and CI on `postgres:18`).
9. Keys: internal `bigint` identity everywhere (except `settings` and the audit log), `public_id` UUID on what
   people reference; internal keys never leave the database.

## Next

A second review pass on the whole file, then it becomes `migrations/0001_schema.sql`.
