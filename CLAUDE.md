# mh3sp-rs

@../docs/CLAUDE.md

The server (`core`). Plans: `roadmap/` (`00. global.md` first). Lasting decisions: `../docs/architecture.md`.

## Rules

- The schema is `migrations/0001_schema.sql` (its reasoning: `../docs/schema.md`); `scripts/dev.sh --reset` after
  changing it. After changing a query or the schema, `scripts/sqlx-prepare.sh`.
- The CI script is `scripts/ci-tests.sh` (not `scripts/ci.sh`).
