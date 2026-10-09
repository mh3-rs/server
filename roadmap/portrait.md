# Portrait of the project (2026-10-09)

Where mh3sp-rs stands after M0–M5, how it compares with MH3SP, what a playable public server still needs, and
what would make the project stand out. Input for reordering `00. global.md`.

## Done

| Milestone | What | Proven by |
|---|---|---|
| M0 Foundations | workspace (`wire`, `server`, `testkit`), framing and items, 301 packet ids, the corpus and its replay harness, CI | the corpus replays against MH3SP |
| M1 Login funnel | OPN (legacy TLS), LMP, accounts in SQLite (support codes as HMAC, UUID public ids), terms, maintenance mode | consoles; corpus replays; tier 3 |
| M2 FMP solo | the world (12 world servers, gates, cities, quests), content binaries, event content read from `CONTENT_DIR`, time and day/night | consoles; tier 3 |
| M3 Multiplayer | city and quest joins, leader transfer, chat, friends, blacklist, messages stored in the database, the hunter profile fully mapped and validated | consoles; ~31 server tests; relay p99 < 2 ms |
| M4 NatNeg + relay | our own NAT negotiation and UDP relay, `p2p` / `relay` / `hybrid` (symmetric NATs and failed punches go to the relay) | consoles (cuts, relay, hybrid); tier 5 |
| M5 Parity (built) | seats, Player Search, party chat, announcements (also in a hunt), stop with warnings, metrics (JSON, Prometheus), quests byte-identical to the live branch | tests; consoles for announcements, stop, position interval |

Size: server ~8,350 lines, wire ~1,800, testkit ~1,950 + ~2,500 lines of tests; 86 tests; 202 typed packets;
15 recorded scenarios; one binary, one SQLite file, one Docker image.

Left in M5: console runs only (`recordings.md`): s12–s14 against MH3SP, the Rust checks of seats and Player Search,
the hour-long session.

## Against MH3SP

**At parity:** login, the world, cities, quests, chat, friends, content (byte-identical quests), announcements.

**Ahead:**
- **NAT negotiation and relay built in.** MH3SP needs AltWFC's Python 2 NatNeg, which fails behind symmetric NATs
  and Docker (error 11688). We fall back to a relay, transparently.
- **Speed.** A town relay takes ~0.3 ms instead of 50–80 ms: no stalled reads, no interleaved frames, players who
  leave vanish at once (MH3SP: ~30 s).
- **Correctness MH3SP lacks:**
  - Local Info shows the real population.
  - Seats are held by one player and freed.
  - Player Search pages work.
  - Profile ids stay stable across restarts.
  - The hunter profile is validated, so a crafted one can't crash other consoles.
  - Friend requests and messages are stored and delivered later.
- **Operations:**
  - `mh3sp stop 5min` with in-game warnings (in hunts too) and `mh3sp announce`;
  - liveness and readiness probes, JSON and Prometheus metrics;
  - anonymous logs, one SQLite file, no Python.
- **Tests:** replays with declared divergences, scripted multi-console tests, NatNeg tests with simulated NATs.

**Behind, or not done:**
- **Bans.** MH3SP has a hard-coded list of support codes (`BANNED_ONLINE_SUPPORT_CODES`). We have none: `allowed()`
  always says yes.
- **Database choice.** MH3SP can use MySQL and spread over several processes (`cache.py` on the live branch). We
  have one process and SQLite, on purpose: it holds hundreds of players.
- **Chat logs.** The live branch keeps them; we don't (D6: moderation decides later).
- **A second event rotation for one world server** (the live branch's "whitelist"). Not taken.
- **The same gaps in both:** joining from a list (city search, Change Gates) and `LayerTell`. Neither server
  answers `ReqLayerJump*`.
- **Japanese version.** Neither handles it (MH3SP only has the EC patcher).

## Must do: a playable public server

What stands between today and strangers playing on it, in order:

1. **Finish M5 on consoles.** s12–s14, the Rust checks, the hour-long session (2–3 consoles).
2. **Real-world networking.** These are the risks no lab run has covered yet:
   - a console on another machine and network (the mixed city);
   - a real Wii, not only Dolphin (legacy TLS on real hardware);
   - consoles at players' homes, behind home routers.
3. **US version (`RMHE08`).** Recognised at login, never played. Everything so far is PAL: one lab run is needed.
4. **How players get here.** Without this nobody can connect:
   - Nintendo login (NAS): AltWFC's (Python 2) deployed next to the server, or our own (small: `/ac`
     `acctcreate` / `login` / `svcloc`). Our own is the clean path and removes Python from the deployment.
   - The patch: Riivolution XML (and an ISO patcher) pointing the disc at our domain, the certificate, and a player
     guide.
   - DNS for the domain and its NatNeg hostnames, a host with a public IP and host networking.
5. **Minimum moderation.** Banning a console by Capcom ID or support code, at runtime: the `allowed()` body, a
   `bans` table, and `mh3sp ban <capcom id>` over the health port, like `announce`. Without it, the first griefer
   means a restart and a code change.
6. **Release pipeline.** Tag → images `1`, `1.0`, `1.0.0` (+ `-prometheus`). From then on, forward-only migrations.
7. **Ops basics.**
   - Back up the SQLite file (a `sqlite3 .backup` cron, or Litestream).
   - Write a deployment example: compose with host networking, the content folder, the secret.

Everything else (the website, anti-cheat, jumps) is optional for a first public server.

## Could do: standing out

From most to least visible to players:

- **The website** (M7–M8). This is the project's showcase: no MH3-era server has one.
  - **Live world:** who plays where, cities and quests filling up, at a glance.
  - **Hunter pages:** HR, weapon, armour and decorations, already parsed from the profile block. Friends, and the
    Capcom ID to share.
  - **Event calendar:** the rotation and Jhen Mohran windows, past and future, from the content files. Admins edit
    the rotation there.
  - **Social:** messages, friend requests and invites written on the site and delivered in game. Kicks by a city's
    host or a quest's leader (the game has none).
  - **Accounts and pairing:** a 6-digit code links a console to an account. Optional Discord login (where the
    community is).
  - **Moderation:** bans, flags with evidence, an audit log.
  - **Public stats:** players over time from the metrics (Prometheus/Grafana).
- **Gameplay the original never had:**
  - Joining a quest after departure (research first).
  - Blocks keeping players apart.
  - City search and Change Gates that actually jump (the "jumps" step; broken everywhere today).
- **Anti-cheat** (M6):
  - decode `NtcCheatCheck`;
  - sample memory checks for Gecko codes;
  - the profile validation already done is the first layer.
- **Reach:** the Japanese version (`RMHJ08`); our own NAS (one stack, no AltWFC).
- **Quality:** scripted Dolphin end-to-end tests as a release check; a public "how it compares" page (latency, relay,
  fixes) for the community.

## Suggested order

1. Close M5 (console runs).
2. **M6 becomes "Public release":**
   - bans;
   - the US lab run and the remote/real-Wii runs;
   - NAS (AltWFC first, ours if small);
   - the patch and its guide;
   - release pipeline, backups, deployment example;
   - then `1.0.0`.
3. **M7–M8 website**, starting with what players see: the live world, hunter pages, the event calendar. Then
   accounts and moderation.
4. Anti-cheat research, the jumps step, joining after departure, the Japanese version: as interest goes.
