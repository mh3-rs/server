-- mh3sp database schema.
--
-- Before 1.0.0 this is the only migration and it is rewritten in place: any change means resetting the database
-- (`scripts/dev.sh --reset`). From 1.0.0 on, migrations only go forward. The reasoning behind each table is in
-- roadmap/M1 schema.md.
--
-- Conventions: internal `bigint` identity keys never leave the database; what people reference has a `public_id`
-- UUID v7. Enumerations are text with a CHECK. Texts and terms are overrides of defaults kept in the code.

-- Key/value settings shared by every node, editable by admins. Values are validated by the server when read (a
-- generic table can't check them). Known keys (a list in the code):
--   natneg_hostname  the only accepted NAT negotiation hostname
--   registration     'open' | 'closed'
--   whitelist        'off' | 'on_register' | 'admin'
CREATE TABLE settings (
    key         varchar(32) PRIMARY KEY,
    value       text        NOT NULL,
    updated_at  timestamptz NOT NULL DEFAULT now()
);

-- Overrides of the texts the server sends (except the terms). Empty by default.
-- Lookup: (key, console language) -> (key, 'en') -> default in the code.
CREATE TABLE texts (
    id          bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    key         varchar(64) NOT NULL,
    language    varchar(8)  NOT NULL,
    value       text        NOT NULL,
    updated_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (key, language)
);

-- Overrides of the terms and sub-terms; each edit is a new row whose id is the version sent to the console
-- (a u32 on the wire). The built-in terms in the code are version 1, so overrides start at 2.
-- Current: latest row for (kind, console language) -> latest for (kind, 'en') -> built-in.
CREATE TABLE terms (
    id          bigint      GENERATED ALWAYS AS IDENTITY (START WITH 2) PRIMARY KEY
                            CHECK (id <= 4294967295),
    kind        text        NOT NULL CHECK (kind IN ('terms', 'subterms')),
    language    varchar(8)  NOT NULL,
    value       text        NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX terms_current ON terms (kind, language, id DESC);

-- The game's four server types and their HR limits.
CREATE TABLE server_types (
    id      bigint   GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    type    text     NOT NULL UNIQUE CHECK (type IN ('open', 'rookie', 'expert', 'recruiting')),
    hr_min  smallint NOT NULL CHECK (hr_min BETWEEN 0 AND 999),
    hr_max  smallint NOT NULL CHECK (hr_max BETWEEN hr_min AND 999)
);

-- World servers of the server list. `position` is only the display order.
CREATE TABLE world_servers (
    id              bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    public_id       uuid        NOT NULL UNIQUE DEFAULT uuidv7(),
    position        smallint    NOT NULL UNIQUE,
    name            text        NOT NULL UNIQUE,
    server_type_id  bigint      NOT NULL REFERENCES server_types,
    capacity        integer     NOT NULL DEFAULT 2000 CHECK (capacity > 0),
    gate_count      smallint    NOT NULL DEFAULT 40 CHECK (gate_count BETWEEN 1 AND 40),
    gate_capacity   integer     NOT NULL DEFAULT 100 CHECK (gate_capacity > 0),
    city_count      smallint    NOT NULL DEFAULT 40 CHECK (city_count BETWEEN 1 AND 40),
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

-- Fixed public ids, so every node and every reset agrees on them.
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
