# mh3sp server

@../docs/CLAUDE.md

The server (`core`). Plans: the `Roadmap` board of the `mh3-rs` organization on Gitea; lasting decisions (ADRs) and the
data model: the wiki of `mh3-rs/docs`.

## Rules

- The schema is `migrations/0001_schema.sql` (its reasoning: the wiki's `Project`); `scripts/dev.sh --reset` after
  changing it. After changing a query or the schema, `scripts/sqlx-prepare.sh`.
- Don't touch `migrations/0001_schema.sql` for anything but a schema change, a comment included: `sqlx::migrate!` checks
  the checksum of a migration a database already applied (assumed, not tried), so an existing database, ours included,
  would refuse to start. Its two old comments citing removed docs wait for the schema review (mh3-rs/server#9).
- A worktree where `scripts/ci.sh --woodpecker` ran keeps root-owned files in `target/ci` (the pipeline runs in Docker):
  `git worktree remove` can't delete it, so remove such a worktree with `sudo`.
- On the shared server the whole test run is timing-sensitive: under a heavy load (`uptime`) unrelated tests fail
  once and pass alone. Run `RUST_TEST_THREADS=4 scripts/ci.sh`, and rerun a failing test by name before
  suspecting the code.
- `mh3sp check <domain>` is for a machine outside the server's network: from inside, a split-horizon DNS or the
  router's loopback gives answers a console would never see (the homelab resolves `pband.ch` to `192.168.1.50`).
