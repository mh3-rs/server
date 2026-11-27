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
- The world checks itself in the tests: the `invariants` feature (on for `testkit` only, never in the image) runs
  `World::check()` whenever the world's lock is released, and `Running::violations()` is what a test asserts on. A change
  to the world's types (`Layer`, `Member`, `Circle`, `City`) updates `World::check` in the same step, and a test that
  breaks one invariant by hand needs `Place.at`, which is `pub(crate)` for that.
- A check on data that a message can write (a NAT negotiation's sockets, a world field) is designed by listing every
  writer of that data, not only its reader: the report check of `natneg.rs` was bypassable through an INIT that
  wrote the same sockets, and only the review saw it.
- A test must not prove that something happened by reading the log through a thread-local subscriber (`Log`): it lost
  every event in 5 of 8 runs when other tests ran in parallel. Observe the effect (a task of the test's own on
  `Running::tasks()`, a frame, the world) and keep the log for what can't be seen otherwise.
- `mh3sp check <domain>` is for a machine outside the server's network: from inside, a split-horizon DNS or the
  router's loopback gives answers a console would never see (the homelab resolves `pband.ch` to `192.168.1.50`).
- `mh3sp check` gets a real answer from each game port (the healthcheck: `wire::healthcheck` for the PAT roles, NatNeg
  records `0x70`/`0x71` for UDP 27901; the glossary's Healthcheck and Check, the wiki's Protocol notes), so a bare
  connect that closes stays a `WARN` and only our own check leaves none. The PAT healthcheck is read in the 250 ms
  pause before the server's first frame (`net.rs`): a change to that pause or to what is read in it keeps the test that
  a frame sent in the pause leaves it at 250 ms.
- Code the server and `testkit` both need (the Wii's TLS client, ids, records) goes in `wire`: `testkit` has `server` as a
  dev-dependency only, and `server::tls` is private.
