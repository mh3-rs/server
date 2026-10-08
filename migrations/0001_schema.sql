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

-- World servers, server types (with their HR limits) and the world layout are not in the database: they are game
-- constants in the code, optionally overridden by `config.toml` at startup.

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

-- Initial settings (everything else has its defaults in the code).
INSERT INTO settings (key, value) VALUES
    ('registration', 'closed'),
    ('whitelist',    'off');
