# Architecture and reference

The lasting part of the roadmap: the reference policy, the facts the game imposes, the architecture decisions
(numbered: code and milestones cite them), testing, traps. The status and what is left: `00. global.md`.

## Reference policy

- **The game is the hard constraint.** What the console sends and accepts (packet layouts, magic bytes, field
  order, quirks like `FallthroughBug`, timings) must be matched exactly: the game cannot be changed.
  Unknown bytes stay opaque and are reproduced until we understand them.
- **Python's server-side choices are not.** Data model, concurrency, error handling, ID generation, world layout,
  what gets persisted, default texts, bugs and limitations: diverge freely wherever a better design exists.
- **Every divergence on the wire is declared.** When the Rust server deliberately answers differently from
  Python, the difference is recorded in `corpus/divergences.toml` (packet, what differs, why). The replay
  harness accepts declared divergences and fails on undeclared ones, so "different on purpose" never hides
  "different by mistake".
- When Python and the game disagree (a Python bug the game tolerates), the game wins, and the corpus records
  the reason in the divergence entry.
- **What isn't built answers "nothing" when it can** (M5 D15): a request the game may send for a feature we don't
  build, and that isn't critical, gets a harmless empty answer rather than a dropped connection.

## Goals

- Byte-compatible with the real game (PAL `RMHP08` first, then `RMHE08`, `RMHJ08` later).
- One binary, every server role, `tokio` async.
- Protocol layer separated from game logic, so logic is testable without sockets.
- Built-in NAT negotiation + UDP relay (strategies: `p2p` | `relay` | `hybrid`).
- **Fast relays.** Python MH3SP takes ~50 ms (median, up to ~70 ms) to relay a player's town position to another
  player on the same machine (measured from `s04`–`s06`; its stalled reads keep relayed frames waiting for the next
  packet, and the lab ran it with per-packet debug logging). Target: p99 under 2 ms on one host, measured by a
  tier 3 test from M3 on (`relay_latency`; 10 ms in CI, whose shared runner alone once crossed 2 ms).

## Non-goals (for now)

- Nintendo login (NAS) and the GameSpy services the game uses: AltWFC for now (our own NAS is a future step).
- Offline content tooling (`dump_quest.py`, `dump_trading_post.py`): the JSON they produce is consumed as is.
- Python 2 anything.
- The RFP server: the game never connects to it (verified by capture, see Facts).

## The world, by level

As the server implements it (MH3SP's numbers). Each level below a world server exists only while players are in it
(M2 D3). Fixed by the game: 4 players per city and per quest; very likely 40 gates and 40 cities (the console itself
asks for 40 entries per list, shown 10 per page). The rest is the server's choice: the 12 world servers, their names
and types, the HR ranges of the types (sent in content binary `0x01`), the capacities 2000 and 100. 4 quests per city
follows from a rule: a player is in one quest at most (created or joined), and a quest ends with its leader.

| Level | How many | Capacity | Names | Notes |
|---|---|---|---|---|
| Server (one process) | 1 | — | — | every role: OPN, LMP, FMP |
| World server | 12 | 2000 players | Valor1–4 (Open), Beginners1–2 (Rookie, HR ≤ 30), Veterans1–2 (Expert, HR ≥ 31), Greed1–4 (Recruiting) | separate worlds: players never meet across them |
| Gate | 40 per world server | 100 players | City Gate1–40 | a lobby |
| City | 40 per gate | 4 players | City1–40 | created by a player, the host of its P2P session; from reservation until its last player leaves |
| Quest (circle) | up to 4 per city | up to 4 players (set at creation) | 1–4 | a leader (only it departs or sets its details: a member asking is refused and logged, `warn`), an optional password, kicks; until its leader leaves |

Open: a gate holds 100 players while its 40 cities could hold 160. MH3SP takes a player entering a city out of its
gate's count; the Rust server counts them in the gate. The multiplayer replays (M3) tell which the console expects.

## Facts established by live capture (2026-10-08)

Captured with two Dolphin instances against MH3SP `dev` + native AltWFC.

- Login = 6 TCP connections:
  `OPN :8200 (TLS)` → `LMP :8201` (LoginInfo, Ticket, UserList, ServerTime) →
  `LMP :8201` again (UserObject, Ticket, FmpList, FmpInfo ×2) →
  `FMP :8202` staging (time, binaries, `63xx` FmpList, FmpInfo, LayerEnd) →
  `FMP :8202` real session (gates, cities, quests).
- The stage is determined by the port and by `NtcLogin` status (OPN 5, LMP 1 or 2, FMP 3).
- Packet categories are not servers: all 0x61 "Rfp" packets arrive on LMP's port; `ReqRfpConnect` is never sent.
- `ReqCommonKey` is answered with `AnsAuthenticationToken` (skips the encryption step) and the game accepts it.
- **Town and quest gameplay are peer-to-peer UDP (GameSpy GT2).** The server matchmakes and relays the
  connection offer (`NtcLayerBinary2` / `NtcCircleBinary2`, which carry the host `IP:port`), then the consoles do
  GameSpy NAT negotiation (UDP 27901) and talk directly.
- NAT negotiation behind Docker/NAT rewriting hands out unreachable addresses → error 11688 (`0x80030042` in `NtcCollectionLog`).
- P2P traffic, 2 players, lobby + quest start: ~5.5 pkt/s, ~42 B payload, ~0.23 kB/s per direction.
- The PAL game registers with GameSpy as `mh3uswii`.
- The online support code lives in the save's `nocopy/network` file; Capcom ID = server lookup on (support code, slot).
- OPN's second `ReqMemoryCheck` (`0x806311a8` on PAL, `0x806308e8` on US) reads the **NAT negotiation hostname
  compiled into the game** (`natneg1.gs.nintendowifi.net` on an AltWFC-patched disc): that is how the server tells
  which network service the disc was patched for.

## Python MH3SP issues not to reproduce

Found while building the corpus and the replay harness. Each one is a place where the Rust server does better.

- **Stalled reads**: the network loop reads one frame per socket readiness event from a buffered reader
  (`mh/server.py`, `makefile('rb', -1)`), so frames arriving together stay in the buffer until more data comes in.
  The game copes because later traffic unblocks it; answers are still delayed. Rust: read until the buffer is empty.
- **Per-process player ids**: `AnsUserSearchInfoMine`'s two ids (used by the game to tell players apart in relayed
  town packets) come from Python's `hash()`, which is randomly salted per process: they change on every restart.
  Rust: stable ids.
- **No `SO_REUSEADDR`**: a restarted server fails with "Address already in use" until the OS releases the ports.
- **Interleaved writes**: `send_packet` writes a frame's header and payload in two `write()` calls, and broadcasts
  write to other players' sockets from other worker threads; the per-connection lock meant for this (`self.rw`) is
  never used. With two players, frames sometimes interleave on the wire (a console then reads garbage such as
  packet id `ff000001`). Rust: one owner per socket, whole frames only.
- **Vanished players linger ~30 s**: a closed socket is only noticed when the next line check fails, so the other
  players see someone who left up to half a minute ago. Rust: react to the closed socket at once, keep the line
  check as a fallback.

## References

| Source | Use |
|---|---|
| `../reference` (branch `dev`) | The oracle. `mh/pat.py`, `fmp_server.py`, `mh/pat_item.py`, `mh/constants.py`, `mh/database.py`, `mh/quest_utils.py` |
| `../lab/` | Test rig: `server.sh`, `altwfc.sh`, `play.sh <rvz> [profile] [mac]`, `hosts.sh`, `shot.sh` |
| `../lab/mhtrida` ([MHTrIDA](https://github.com/sepalani/MHTrIDA)) | Packet table with JP names, error codes, save format + checksum, quest files, symbol maps, Wireshark dissector |
| `../lab/mhtriserver` ([MHTriServer](https://github.com/InusualZ/MHTriServer)) | C#: one typed class per packet, a model for the Rust structs |
| [mhtri-decomp](https://github.com/InusualZ/mhtri-decomp) | Game decompilation (`src/Network`, `src/DWCi/DWCi_NatNeg.c`): ground truth for client behaviour |
| `../lab/altwfc/gamespy_natneg_server.py` | NAT negotiation protocol (commands 0x00–0x10) |

## Architecture decisions

1. **One binary, one server.** Every role in one process: `opn`, `lmp`, `fmp` TCP listeners and (from M4) the NAT
   negotiation + relay UDP service, always all on. One server handles the
   whole community (hundreds of players; gameplay is console-to-console).
2. **Three layers, strict direction:**
   - `wire`: framing (`>HHI` size/seq/id header), typed items, `PatData` records, one Rust type per packet.
   - `game`: the logic, `async fn handle(&mut session, &shared, &Frame) -> Result<Vec<Frame>, Fault>`: no
     sockets; it reads and writes the database (terms, accounts), and `Fault` says whose fault a dropped
     connection is (`warn` for the console's, `error` for the server's). The frames are replies to this
     connection, in order (a notification to the requester itself goes there too, so it keeps its place
     relative to the answer); frames for other players go to their mailbox (`game::players`), a bounded channel
     their connection task drains. No `Effect` type: it would only reorder the same two paths.
   - `net`: tokio listeners, per-connection task, executes `Effect`s.
3. **Opaque bytes stay opaque.** Unknown fields (`unk_*`, magic constants) are kept as raw values in the
   structs and written back byte for byte. No "cleanups" before the replay harness proves equivalence.
4. **What lives where.**
   - **Memory:** the live world (world servers, gates, cities, circles, players, positions) and every piece of
     temporary data: sessions and PAT tickets (handed off between connections, see below), pairing codes
     (~10 minutes), presence, populations. Lost on restart, which only affects people connected at that moment.
     Data lives with what owns it:
     - a connected console's session (Capcom ID, slot, language, region, …) is owned by its connection task;
     - between connections (OPN → LMP → LMP → FMP) it is handed off through a `moka::sync::Cache` keyed by PAT
       ticket, with a 2-minute TTL: put in at `ReqShut` (before answering, so the next connection can't come
       first), taken out (removed) by the next connection presenting the ticket. Nothing to refresh, and a ticket
       works once. Pairing codes (M8) use the same kind of cache, 10 minutes;
     - other connections reach a player through a handle in a `DashMap<CapcomId, Player>` (M2): its frame
       channel (`mpsc::Sender`) and the public info others need; the socket stays with its task, the only
       writer;
     - fixed-shape state (the world layout: world servers, gates, cities): a struct behind a `std::sync::Mutex`.
     Locks are `std::sync::Mutex`, never held across an `.await` (no tokio mutex).
   - **SQLite:** what must survive: accounts (consoles, hunters), terms overrides, and later friends, bans,
     settings and text overrides (M8).
   - **Content files** (`CONTENT_DIR`): event quests, rotation and trading post, MH3SP's JSON files (M2).
   - **Environment variables:** deployment settings (addresses, certificate, database file, external address,
     accepted NAT hostname).
5. **Database: SQLite through `sqlx`** (bundled SQLite, no database server). One file
   (`data/mh3sp.db`), in WAL mode so readers never block the writer, also across processes: a website (M7) runs on
   the same machine and shares the file. Foreign keys enabled on every connection; `STRICT` tables. Internal
   `INTEGER PRIMARY KEY` ids, `public_id` UUID v7 (generated by the server) for what people reference (see
   `M1 schema.md`). Migrations with `sqlx migrate` (`migrations/`, embedded in the binary); compile-time checked
   queries with the offline cache (`cargo sqlx prepare`, `.sqlx/` committed) so CI builds without a database.
   **Schema before 1.0.0**: one file (`migrations/0001_schema.sql`) rewritten in place; a schema change resets the
   database (dev, CI, beta). **From 1.0.0**: forward-only migrations, never edited, no down scripts.
6. **TLS:** `openssl` crate (`tokio-openssl`) with `@SECLEVEL=0`, legacy protocol versions and
   `AES128-SHA`/`AES256-SHA`/RC4/DES ciphers. `rustls` cannot speak to a real Wii (1024-bit CA, TLS ≤ 1.0, RSA key exchange).
7. **Config**: the server listens on `0.0.0.0`, on `OPN_PORT`, `LMP_PORT`, `FMP_PORT`, `HEALTH_PORT` (default 8200,
   8201, 8202, 8280). Consoles are handed the LMP and FMP ports it listens on, so a container publishes them
   unchanged: ports are changed here, not in the port mapping. Everything else comes from environment variables,
   each with a default (`crates/server/src/config.rs`): the TLS certificate (`TLS_CERT`, `TLS_KEY`), `DATABASE_URL`,
   `RUST_LOG`; from M1 `EXTERNAL_ADDRESS`, `NATNEG_HOSTNAME` and `SECRET` (the support-code pepper; or `SECRET__FILE`,
   read from a file such as a Docker secret, which takes precedence). Names
   carry no prefix: the container's environment is the server's alone. A `.env` file is loaded when present
   (dev); in production the container gets them from compose. No config file.
8. **Logging:** `tracing` + `tracing-subscriber` from the first line of server code. Terminal output first;
   a file (`tracing-appender`) and JSON for production can be added later without touching call sites.
   Filtering with `RUST_LOG` (`EnvFilter`), default `info`. Library crates (`wire`) don't log: they return errors
   and the caller picks the level.

   | Level | Meaning | Examples |
   |---|---|---|
   | `error` | The server is at fault and a human should look, crash or not | database unusable (file missing, disk full), broken internal invariant (a bug), a listener died |
   | `warn` | Something went wrong and we recovered, or something needs to stand out | malformed packet → connection dropped, unexpected packet for the current stage, P2P failure falling back to relay, every anti-cheat detection or action |
   | `info` | What happens on the server, in player terms | login, Capcom ID chosen, enters gate/city, creates/joins/starts/ends a quest, disconnects; startup/shutdown |
   | `debug` | Protocol flow and decisions, no raw bytes | one line per packet (name, seq, size), state transitions, why a decision was taken (relay strategy), database operations |
   | `trace` | High-volume details | raw packet hexdumps, positions and town relays, cheat-check payloads, P2P/relay datagrams |

   Rules:
   - **Nothing a client does is `error`.** Clients are untrusted: malformed or hostile input is `warn` at most,
     and one `warn` per connection before dropping it, so a hostile client can't flood the logs.
   - **Context lives in spans, not in messages.** One span per connection (`conn` = internal connection number,
     `role`), with `capcom_id` recorded into it once known; nested spans for `world`, `city`, `circle` around the
     code handling them. Field names are fixed across the codebase (`capcom_id`, never `cid` or `user`).
   - **Logs are anonymous: IDs only, at every level.** Never hunter names, IP addresses, chat or friend-request
     text, or anything else typed by or identifying a person. Moderation works by `capcom_id` / support code,
     which the server already holds.
   - **Secrets are never logged, at any level:** PAT tickets (session tokens) and online support codes
     (account credentials in practice). Where one must be correlated, log a short hash.
   - Messages are short and factual (`"joined city"`), the structured fields carry the data.

9. **Health: liveness and readiness are separate.** A small HTTP endpoint (no dependency):
   - `/health/live`: 200 when the runtime answers. This is the container `HEALTHCHECK`
     (`mh3sp healthcheck`, no `curl` in the image); failing it justifies a restart.
   - `/health/ready`: 200 only when the database answers `SELECT 1` within 2 s, else 503 with the reason.
   - At startup, a database that can't be opened doesn't stop the server: it logs the error, retries in a loop, and stays
     **unhealthy** until it is up, the migrations have run and the server has become ready once (nobody can be
     connected yet, so a restart loses nothing). After that, a database failure (with a local file: disk full,
     corruption, a file removed) only makes it **not ready**, logged at
     `error`: players already in a gate, a city or a quest keep playing (all of that lives in memory; hunts are
     console-to-console), players trying to join get the "not ready" text in the game, and actions that need the
     database mid-game fail cleanly (an in-game message, the player stays connected). A restart would disconnect
     everyone and fix nothing. (Decided in M1; M0's skeleton still exits at startup.)

10. **Deferred features hide behind a function the code already uses**: when a feature is simplified now and
   completed later, its callers go through a real function with its final signature, and only the body changes
   later. Texts: `text(Text, Language) -> Cow<'static, str>`, a `match` returning English today (the language
   ignored, the `Cow` borrowed), database overrides and translations in M8. Access: `allowed(console) -> bool`,
   `true` today, the access modes in M8. Callers never change; nothing to grep for.

11. **Bots: a body in the server, a brain in its own process** ([`experiment-bots.md`](experiment-bots.md)). A quest
    bot is a hunter to the consoles: the server is its body (its party membership in memory, its NAT negotiation, GT2
    and session with the consoles, its messages on the wire); `mh3sp-brain` decides what it does and never touches the
    game's protocol. The server applies the game's rules to the bot (how a move plays out, the hits it takes, its HP,
    its faint and cart, monster handovers); the brain is only strategy: which move, where, when (its own repository, `../brain`; the messages in `../tunnel`, which both pin by commit). They talk
    over one TCP connection kept open for every bot (localhost, `BOT_PORT`; no setting, no
    bots), in length-prefixed binary frames: commands (create, leave, behaviour) and the party's events (quests,
    hunters, monsters, area changes) from the server, actions (move, change area, attack) from the brain. A bot is
    named by its Capcom ID everywhere (generated by the server, never a player's), and events are addressed per party.

## Dev environment

- `scripts/dev.sh`: starts the dependencies below and creates/completes `.env` (idempotent).
- `scripts/ci-tests.sh [--docker | --woodpecker]`: runs exactly what CI runs (natively, in the CI image, or the
  real pipeline files through `woodpecker-cli exec`, services included).
- No services to start: the database is a file. `scripts/dev.sh` creates `.env` and `data/`;
  `scripts/dev.sh --reset` deletes the dev database (needed after a schema change before 1.0.0).
- The server runs with `cargo run -p server` (environment variables, `.env` loaded when present); migrations run at
  startup. It needs ports 8200–8202, like Python MH3SP: stop one to run the other (`tools/oracle.sh` restarts
  Python when a replay needs it).
- `Dockerfile`: the release image (Debian slim + OpenSSL 3, non-root, `HEALTHCHECK` on `/health/live`).
- Game side: the lab in `../lab` (Dolphin profiles, hosts toggle, native AltWFC, Python MH3SP as oracle).
- AltWFC / NAT negotiation never runs behind Docker port mapping (it rewrites client addresses, P2P fails).

## Testing strategy

Python MH3SP is the reference (see Reference policy). An undeclared diff against it is a finding, not a
nuisance: either reverse-engineering knowledge the port would otherwise lose, or a divergence to declare.

**Corpus.** Scenarios played in the lab against Python MH3SP and recorded from **Dolphin's network captures**
(`DumpAsPCAP` + SSL dumps): every TCP connection separately, both directions, timestamps, OPN's TLS already
decrypted, plus the UDP traffic (NAT negotiation, P2P). Imported into `corpus/<scenario>/`. Volatile fields
(tickets, support codes, Capcom IDs, timestamps) are found by recording a scenario twice and masked per packet type.
Details in `M0 foundations.md`.

| Tier | What | Runs where | Since |
|---|---|---|---|
| 1. Unit | `wire` round-trips (`decode(encode(x)) == x`, `encode(decode(bytes)) == bytes` on every corpus packet), property tests on items/`PatData`; `game` logic as pure functions (`Request` → `Effect`s) | CI, every push | M0 |
| 2. Replay | Each recorded scenario's client packets replayed over real sockets against a server; replies diffed against the corpus. Validated first against Python MH3SP | CI, every push | M0 |
| 3. Scripted E2E | Fake consoles (Rust test client crate) over real sockets: TLS on OPN, the full 6-connection login, multi-client scenarios (2–4 clients in a city/quest), disconnects mid-flow, concurrency stress | CI, every push | M1 |
| 4. Live differential | The same scripted scenarios against Python MH3SP **and** Rust, started side by side in CI; diff the conversations | CI, nightly + on demand | later (M1: kept for later) |
| 5. NAT negotiation / relay | Fake UDP peers run NAT negotiation and exchange traffic in `p2p`, `relay`, `hybrid`; simulated NAT (address/port rewriting) and forced failures for the hybrid fallback | CI, every push | M4 |
| 6. Dolphin E2E | Real game in Dolphin, automated: launch with `play.sh`-style config, drive menus, assert progress from server logs and game memory (error code via the PAL symbol maps), screenshots on failure | Self-hosted runner only (needs the game image), nightly / manual | later (M1: kept for later) |

### CI: self-hosted Gitea + Woodpecker

- Pushes to Gitea trigger Woodpecker pipelines (`.woodpecker/*.yml`).
- Secrets available to pipelines: `gitea_user`, `gitea_token` (Gitea registries: Cargo, npm, container).
- `push` pipeline: `fmt`, `clippy -D warnings`, tiers 1–3 and 5, coverage report on tiers 1–3.
  no services: each test gets its own throw-away SQLite file;
  `SQLX_OFFLINE=true` for the build, plus a check that `.sqlx/` is up to date.
  Cache `~/.cargo` and `target/` between runs.
- `nightly` pipeline (Woodpecker cron): tier 4, with Python 3 + a pinned MH3SP checkout.
- `dolphin` pipeline: tier 6 on a Woodpecker agent running on the Mac (local backend, own label), because the
  game image never leaves that machine. Manual or nightly trigger.
- `release` pipeline (on tag): container image built with `woodpeckerci/plugin-docker-buildx:5.0.0` (allowed as a
  privileged plugin) and pushed to the Gitea container registry. The image must run with host networking (or a
  public IP without address rewriting) whenever NAT negotiation is enabled.
- The Cargo registry is only needed if crates get shared between repos (e.g. the test client); not before.

## Traps to remember

- Legacy TLS (see decision 6). Test against real-Wii settings, not only Dolphin.
- Reproduce unknown bytes exactly; MH3SP has many `# TODO: Field??` constants that the game depends on.
- Copy timings: the ~1.5 s connection delay, 30 s line check, client timeouts.
- `str` vs `bytes` is mixed in MH3SP for Capcom IDs and names: decide each field's real type from the corpus.
- The external IP handed out in `AnsLmpConnect` / `AnsFmpInfo` must be reachable by clients (config, not `0.0.0.0` guessing).
- NAT negotiation must see real client addresses: never behind Docker port mapping or a NAT-rewriting load balancer.
- The game never retries P2P by itself: every fallback costs the player one failed attempt.

## Deployment shapes

- **Standalone** (the default): the server and its SQLite file (plus AltWFC until our own Nintendo login), `access =
  open`. The server never depends on the website.
- **With the website**: adds accounts, pairing and the `on_register` / `admin` access modes, and the admin pages.
  Those access modes without a website are a startup `error` (nobody could ever register).

## Open questions

- ~~Quest P2P topology~~: a full mesh, pair by pair, from the city on (M4 D7, lab run 14).
- ~~Whether `NtcLayerBinary2`'s embedded address is used~~: no, the peer's address comes from NatNeg's `CONNECT` (M4).
- `NtcCheatCheck` payload format.
- What the 35.80.180.25 (AWS us-west-2) connection seen from Dolphin was: a host missing from the redirection list?
- JP version specifics (EC/shop check, paid-account packets).

