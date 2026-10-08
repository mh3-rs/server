-- mh3sp database schema.
--
-- Before 1.0.0 this is the only migration and it is rewritten in place: any change means resetting the database
-- (`scripts/dev.sh --reset`). From 1.0.0 on, migrations only go forward. The reasoning behind each table is in
-- roadmap/M1 schema.md.
--
-- Conventions: internal `bigint` identity keys never leave the database; what people reference has a `public_id`
-- UUID v7. Strings are `text` with a CHECK (never varchar(n)); enumerations too. Texts and terms are overrides of
-- defaults kept in the code. Lists of keys (settings, texts) live only in the code: the database checks their
-- format, the server validates their meaning (on read, and through the admin API, the only writer).

-- Key/value settings shared by every node, editable by admins. Known keys (the list is in the code):
--   natneg_hostname  the only accepted NAT negotiation hostname
--   registration     'open' | 'closed'
--   whitelist        'off' | 'on_register' | 'admin'
CREATE TABLE settings (
    key         text        PRIMARY KEY CHECK (key ~ '^[a-z][a-z0-9_]{0,31}$'),
    value       text        NOT NULL,
    updated_at  timestamptz NOT NULL DEFAULT now()
);

-- Overrides of the texts the server sends (except the terms). Empty by default.
-- Lookup: (key, console language) -> (key, 'en') -> default in the code.
CREATE TABLE texts (
    id          bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    key         text        NOT NULL CHECK (key ~ '^[a-z][a-z0-9_.]{0,63}$'),   -- e.g. 'refusal.not_ready'
    -- ISO 639-1: the game's languages are fixed (en, fr, de, it, es; ja later) and have no regional variants.
    language    text        NOT NULL CHECK (language ~ '^[a-z]{2}$'),
    value       text        NOT NULL,
    updated_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (key, language)
);

-- Overrides of the terms and sub-terms; each edit is a new row whose id is the version sent to the console.
-- The console remembers the version it accepted and shows the terms again when the server's differs; ids only
-- grow, so an edit is always shown again.
-- Current: latest row for (kind, console language) -> latest for (kind, 'en') -> built-in.
CREATE TABLE terms (
    -- Starts at 2: the terms built into the code are version 1, so the first override is always newer.
    -- At most 4294967295 (2^32 - 1): the version travels as a 32-bit unsigned field (AnsTermsVersion).
    id          bigint      GENERATED ALWAYS AS IDENTITY (START WITH 2) PRIMARY KEY
                            CHECK (id <= 4294967295),
    kind        text        NOT NULL CHECK (kind IN ('terms', 'subterms')),
    language    text        NOT NULL CHECK (language ~ '^[a-z]{2}$'),           -- ISO 639-1, as in `texts`
    value       text        NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX terms_current ON terms (kind, language, id DESC);

-- The game's four server types and their HR limits. Both bounds are inclusive (SQL's BETWEEN is, and so is the
-- game's wording: Rookie "HR 30 or lower", Expert "HR 31 or higher").
CREATE TABLE server_types (
    id      bigint   GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    type    text     NOT NULL UNIQUE CHECK (type IN ('open', 'rookie', 'expert', 'recruiting')),
    hr_min  smallint NOT NULL CHECK (hr_min BETWEEN 0 AND 999),
    hr_max  smallint NOT NULL CHECK (hr_max BETWEEN hr_min AND 999)
);

-- World servers of the server list: configuration, not live data (who is where lives in node memory and the
-- cache). Admins add, rename, reorder and disable them (website, M7); nodes reload on a cache message. A server
-- with players is disabled, not deleted. `position` is only the display order. The layout of a world (40 gates of
-- 100 players, 40 cities of 4 per gate, as in the game) is a constant in the code.
CREATE TABLE world_servers (
    id              bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    public_id       uuid        NOT NULL UNIQUE DEFAULT uuidv7(),
    position        smallint    NOT NULL UNIQUE,
    name            text        NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 24),  -- max to check in FmpListData
    server_type_id  bigint      NOT NULL REFERENCES server_types,
    capacity        integer     NOT NULL DEFAULT 2000 CHECK (capacity > 0),  -- players, shown in the server list
    enabled         boolean     NOT NULL DEFAULT true,
    created_at      timestamptz NOT NULL DEFAULT now()
);

-- A console, identified by HMAC-SHA256(pepper, online support code); the code itself is never stored.
CREATE TABLE consoles (
    id                  bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    public_id           uuid        NOT NULL UNIQUE DEFAULT uuidv7(),
    support_code_hmac   bytea       NOT NULL UNIQUE CHECK (length(support_code_hmac) = 32),
    region              text        NOT NULL CHECK (region IN ('RMHP08', 'RMHE08')),
    created_at          timestamptz NOT NULL DEFAULT now(),
    last_seen_at        timestamptz NOT NULL DEFAULT now()
);

-- One of a console's 6 Capcom ID slots (an online identity a hunter of the save plays under).
-- Another hunter taking the slot overwrites name and profile and drops its social links; the Capcom ID and its
-- moderation records stay.
CREATE TABLE hunters (
    id              bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    public_id       uuid        NOT NULL UNIQUE DEFAULT uuidv7(),
    console_id      bigint      NOT NULL REFERENCES consoles ON DELETE CASCADE,
    slot            smallint    NOT NULL CHECK (slot BETWEEN 1 AND 6),
    capcom_id       text        NOT NULL UNIQUE CHECK (capcom_id ~ '^[0-9A-Z]{6}$'),
    name            bytea       NOT NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    last_seen_at    timestamptz NOT NULL DEFAULT now(),
    UNIQUE (console_id, slot)
);
CREATE INDEX hunters_console ON hunters (console_id);

-- Reference data (texts and terms have their defaults in the code, not here).

INSERT INTO server_types (type, hr_min, hr_max) VALUES
    ('open',       0, 999),
    ('rookie',     0,  30),
    ('expert',    31, 999),
    ('recruiting', 0, 999);

-- The starting set. Fixed public ids, so every node and every reset agrees on them; servers added later by admins
-- get generated ids.
INSERT INTO world_servers (public_id, position, name, server_type_id)
SELECT w.public_id::uuid, w.position, w.name, t.id
FROM (VALUES
    ('0199c4a0-0000-7000-8000-000000000001',  1, 'Valor1',     'open'),
    ('0199c4a0-0000-7000-8000-000000000002',  2, 'Valor2',     'open'),
    ('0199c4a0-0000-7000-8000-000000000003',  3, 'Valor3',     'open'),
    ('0199c4a0-0000-7000-8000-000000000004',  4, 'Valor4',     'open'),
    ('0199c4a0-0000-7000-8000-000000000005',  5, 'Beginners1', 'rookie'),
    ('0199c4a0-0000-7000-8000-000000000006',  6, 'Beginners2', 'rookie'),
    ('0199c4a0-0000-7000-8000-000000000007',  7, 'Veterans1',  'expert'),
    ('0199c4a0-0000-7000-8000-000000000008',  8, 'Veterans2',  'expert'),
    ('0199c4a0-0000-7000-8000-000000000009',  9, 'Greed1',     'recruiting'),
    ('0199c4a0-0000-7000-8000-00000000000a', 10, 'Greed2',     'recruiting'),
    ('0199c4a0-0000-7000-8000-00000000000b', 11, 'Greed3',     'recruiting'),
    ('0199c4a0-0000-7000-8000-00000000000c', 12, 'Greed4',     'recruiting')
) AS w (public_id, position, name, type)
JOIN server_types t ON t.type = w.type;

INSERT INTO settings (key, value) VALUES
    ('registration', 'closed'),
    ('whitelist',    'off');
