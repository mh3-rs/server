# mh3sp server

@../docs/CLAUDE.md

The server (`core`). Plans: `roadmap/` (`00. global.md` first). Lasting decisions: `../docs/architecture.md`.

## Rules

- The schema is `migrations/0001_schema.sql` (its reasoning: `../docs/schema.md`); `scripts/dev.sh --reset` after
  changing it. After changing a query or the schema, `scripts/sqlx-prepare.sh`.
- The CI script is `scripts/ci-tests.sh` (not `scripts/ci.sh`).
- On the shared server the whole test run is timing-sensitive: under a heavy load (`uptime`) unrelated tests fail
  once and pass alone. Run `RUST_TEST_THREADS=4 scripts/ci-tests.sh`, and rerun a failing test by name before
  suspecting the code.
- `mh3sp check <domain>` is for a machine outside the server's network: from inside, a split-horizon DNS or the
  router's loopback gives answers a console would never see (the homelab resolves `pband.ch` to `192.168.1.50`).
