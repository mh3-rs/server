# mh3sp server

@../docs/CLAUDE.md

The server (`core`). Plans: the `Roadmap` board of the `mh3-rs` organization on Gitea; lasting decisions (ADRs) and the
data model: the wiki of `mh3-rs/docs`.

## Rules

- The schema is the migrations in `migrations/` (its reasoning: the wiki's `Project`), forward only: the image is public,
  so a schema change is a new `NNNN_*.sql`, never an edit of one that was released. `0001_schema.sql` is never touched,
  a comment included: `sqlx::migrate!` checks the checksum of a migration a database already applied, so an existing
  database would refuse to start (a test pins its hash: `crates/server/tests/migration.rs`, which also upgrades a
  database written by the published server). After changing a query or the schema, `scripts/sqlx-prepare.sh`; it
  needs the `sqlite3` command line tool.
- A worktree where `scripts/ci.sh --woodpecker` ran keeps root-owned files in `target/ci` (the pipeline runs in Docker):
  `git worktree remove` can't delete it, so remove such a worktree with `sudo`.
- On the shared server the whole test run is timing-sensitive: under a heavy load (`uptime`) unrelated tests fail
  once and pass alone. Run `RUST_TEST_THREADS=4 scripts/ci.sh`, and rerun a failing test by name before
  suspecting the code.
- A scripted test that expects a frame or a notice waits for it (`expect`, `delivered`: seconds), and a fixed window
  (`drain`, `mail`: 200 ms) is only for "nothing arrives": on a loaded host a late frame fails the first kind, and
  `a_database_failure_keeps_the_player` and `messages_wait_for_their_recipient` did until that was fixed. The tests
  that go through the commands' socket (`mh3sp announce`, `stop`) are Linux only.
- `mh3sp check <domain>` is for a machine outside the server's network: from inside, a split-horizon DNS or the
  router's loopback gives answers a console would never see (the homelab resolves `pband.ch` to `192.168.1.50`).
