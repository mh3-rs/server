# mh3sp-rs

@../docs/CLAUDE.md

The server (`core`). Plans: `roadmap/` (`00. global.md` first). Lasting decisions: `../docs/architecture.md`.

## Rules

- A feature deferred hides behind a function with its final signature (architecture decision 10).
- No internal primary keys in users' hands (UUID public ids); support codes stored only as HMAC-SHA256 with the configuration's `secret`.
- The schema is `migrations/0001_schema.sql` (its reasoning: `../docs/schema.md`); `scripts/dev.sh --reset` after
  changing it. After changing a query or the schema, `scripts/sqlx-prepare.sh`.
- The CI script is `scripts/ci-tests.sh` (not `scripts/ci.sh`).
