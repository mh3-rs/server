# M1 — Login funnel (OPN + LMP)

**Status: draft.** To be discussed before work starts, see "Decisions to take first".

From the title screen to the server list, on the Rust server: the console logs in, picks a Capcom ID slot,
receives the world server list and the address of the FMP server for the one it picks. The FMP side (gates,
cities) is M2: in M1 the console reaches FMP and stops there.

## Exit criteria

1. A Dolphin console goes from the title screen to the server list, picks Valor1 and connects to the Rust FMP
   listener (which doesn't answer yet).
2. Every OPN and LMP connection of the corpus replays cleanly against the Rust server, not only `s00` and `s01`:
   the login connections of `s02`–`s06` are the same three sequences (see "Protocol").
3. Tier 3: scripted logins pass (see Tests). Tier 4: the nightly job replays the corpus against the pinned Python
   MH3SP. Tier 6: decided (Dolphin automation built, or dropped with the reason written down).
4. Accounts survive a server restart; sessions survive a hop from one connection to the next.

## Protocol: what the console does

The corpus has 43 login connections; each one follows exactly one of these sequences (`s01` stops OPN at the
terms). Line checks (`ReqLineCheck` from the server every 30 s, `AnsLineCheck` back) can interleave anywhere.

**OPN** (TLS, port 8200), 13 client messages:
```
← ReqMemoryCheck (game id at 0x80000000)        → AnsMemoryCheck  "RMHP08"
← ReqMemoryCheck (NAT negotiation hostname)     → AnsMemoryCheck  "natneg1.gs.nintendowifi.net"
← ReqConnection                                 → AnsConnection   (ConnectionData: support code if any, …)
← NtcLogin (status 5)
→ ReqAuthenticationToken (Nintendo login token) ← AnsAuthenticationToken
→ ReqTermsVersion / ReqTerms                    ← AnsTermsVersion / AnsTerms          (chunked)
→ ReqSubTermsInfo / ReqSubTerms                 ← AnsSubTermsInfo / AnsSubTerms       (chunked)
→ ReqAnnounce                                   ← AnsAnnounce
→ ReqNoCharge                                   ← AnsNoCharge
→ ReqVulgarityInfoLow ×2                        ← AnsVulgarityInfoLow ×2
→ ReqCommonKey                                  ← AnsAuthenticationToken  (MH3SP skips the key exchange)
→ ReqLmpConnect                                 ← AnsLmpConnect (LMP address and port)
→ ReqShut                                       ← AnsShut
```
**LMP, first visit** (port 8201), choosing the Capcom ID slot:
```
← ReqConnection → AnsConnection ← NtcLogin (status 1)
→ ReqLoginInfo        ← AnsLoginInfo     (issues a support code to a first-time console)
→ ReqTicket           ← AnsTicket        (PAT ticket: the session's key for the next connections)
→ ReqUserListHead / ReqUserListData / ReqUserListFoot ← … (the 6 Capcom ID slots)
→ ReqServerTime       ← AnsServerTime
→ ReqShut             ← AnsShut
```
**LMP, second visit**, with the ticket, choosing the world server:
```
← ReqConnection → AnsConnection (with the ticket) ← NtcLogin (status 2)
→ ReqUserObject       ← AnsUserObject    (the chosen slot: existing Capcom ID, or a new one)
→ ReqTicket           ← AnsTicket
→ ReqFmpListVersion / ReqFmpListHead / ReqFmpListData / ReqFmpListFoot ← … (world servers, populations)
→ ReqFmpInfo ×2       ← AnsFmpInfo ×2    (address of the chosen world server's FMP)
→ ReqShut             ← AnsShut
```
Reference: MH3SP `mh/pat.py`, `opn_server.py`, `lmp_server.py`; MHTrIDA `server/doc` (`net_connect_*`,
`set_open_error_code`, `NetworkWiiMediator/*`).

## Decisions

### Settled

- **D1 Texts** (terms, sub-terms, announce, no-charge, refusal and kick messages, vulgarity lists): in PostgreSQL,
  seeded with default values. The console remembers the terms version it accepted and re-shows the terms only when
  the version changes: editing the terms bumps their version. Texts use the game's markup (`<BR>`, `<CENTER>`,
  `<SIZE=n>`, `<C=n>`, `<LEFT>`, `<END>`).
- **D2 World servers** (names, types Open/Rookie/Expert/Recruiting, HR limits, capacities, gates × cities): in
  PostgreSQL, seeded with MH3SP's 12 servers. Every node of a fleet reads the same list.
- **D3 Patch check**: PAL (`RMHP08`) and US (`RMHE08`) accepted; anything else refused with a message. JP is a
  future step (it needs the EC/shop patch). **No public services: players use our patch, which points every
  hostname of the game (Nintendo login, GameSpy, NAT negotiation, Capcom) at our domain.** A patch is needed anyway
  (no DNS change for players), so we own the whole chain, and our NAT negotiation makes the hybrid relay strategy
  possible. The accepted NAT negotiation hostname is ours: a `settings` row derived from the deployment's domain;
  a disc with another name didn't use our patch and is refused with a message pointing to the website. The website
  generates the patch for its own domain (which must fit the replaced strings: `nintendowifi.net` is 16
  characters). Services to run, as observed in the captures: the Nintendo login (HTTP), GameSpy's availability
  check (UDP 27900), NAT negotiation (UDP 27901): AltWFC until our own.
- **D4 Refusals**: one path in the server, the game's own: OPN answers `NtcLogin` status 2, the console asks
  `ReqMaintenance`, the server sends a text. Reasons: not ready, unsupported disc, NAT hostname, banned (M5), not
  whitelisted (M5); texts from the database. **Planned maintenance** is a standalone mode of the same binary,
  `mh3sp maintenance "<message>"`: no database, no Valkey (it only reads `config.toml` for ports and the TLS
  certificate); it listens on OPN (Wii TLS), refuses everyone with that message, and serves `/health/live`. With
  compose: add `command: ["maintenance", "…"]` and `docker compose up -d`; remove it and `up -d` again to come back.
- **D5 The pause before `ReqConnection`**: probably needed (the same issue was met in an earlier attempt), likely
  lower than 1.5 s. Found by bisection against Dolphin: 1.5 s, 0 s, 750 ms, … down to the smallest value that
  works, then kept as a named constant with the reason.
- **D6 Languages**: texts and content keyed by language in the database, `(key, language, value)`, English as the
  fallback; the console sends its language in `AnsConnection`.
- **D7 Generated ids**: the game's formats: support codes 11 characters, PAT tickets 11, Capcom IDs 6, alphabet
  `0-9A-Z` (what MH3SP uses and the game accepts). Drawn from a cryptographic random source; unique (database for
  support codes and Capcom IDs, Valkey for tickets).

### Open

- [ ] **Schema**: drafted in [`M1 schema.md`](M1%20schema.md), under review.
- [x] **V1 Cache**: Valkey is optional. A `Cache` trait with two implementations, chosen at startup: Valkey when
      configured, in-memory otherwise (single node only; the `[valkey]` config section and the compose service are
      optional). Covers JSON values with an expiry, delete, set-if-absent (ownership leases), publish/subscribe.
      Never a source of truth, no persistence; not needed to be live.
- [x] **V1 runtime**: the cache is chosen once at startup and never switched; with Valkey configured and unreachable,
      it's an error and the server is live but not ready (no new players) until Valkey answers again. The handle is
      passed to the code that needs it (not a global: tests run several servers in one process).
- [x] **Dependency outages** (changes decision 9 of the global roadmap):
      - at startup: the server starts anyway, logs the error and retries in a loop; it is **unhealthy**
        (`/health/live` fails) until Postgres and Valkey (when configured) are both up, the migrations have run and
        it has become ready once (nobody can be connected yet, so a restart loses nothing);
      - after that: Postgres or Valkey down makes it **not ready** (no new players; `/health/ready` names the
        dependency), never unhealthy: with an orchestrator, unhealthy means a restart, which would disconnect every
        player in town or on a quest and fix nothing;
      - both outages are logged at `error`: either way players can't join, so a human should look.
- [x] **V2 Key naming**: `mh3sp:<kind>:<id>` (`ticket`, `presence`, `world:<id>:owner`, `node`, `pairing`, …).
- [x] **V3 Sessions**: a ticket's session (Capcom ID, slot, language, region, NAT hostname, stage) expires after a
      short time (about 2 minutes) and is refreshed while its console is connected.
- [x] **V4 Value format**: one JSON value per key, with an expiry.
- [x] **Support code storage**: never in clear; looked up by HMAC-SHA256(pepper, support code), the pepper a server
      secret outside the database (environment), never changed. Support codes are random (~57 bits), so a slow
      password hash (Argon2) brings nothing and can't be looked up; the keyed hash can, and a leaked database alone
      reveals nothing.
- [x] **Pairing** (whitelist, M5/M7): starts from a console connection, never shows a secret; a 6-digit one-time code,
      expiring after ~10 minutes; the website limits attempts per account.
- [x] **Identity**: every entity has a UUID (v7: time-ordered, indexes well); connection spans carry the `console`
      UUID from LMP on, and the `account` UUID once linked.

## Work

### Packets
- [ ] Typed request/answer structs for the 25 client messages above and their answers, decoded with `wire`'s
      `Reader`, checked against every corpus payload (a decode/encode round-trip test per type).
- [ ] Field types decided from the corpus (`str` vs `bytes` mixing in MH3SP): Capcom IDs, tickets and support codes
      are ASCII; hunter names are bytes.

### Connection lifecycle (all roles)
- [ ] Send `ReqConnection` first (after the pause, if one turns out to be needed), `NtcLogin` with the stage's
      status after `AnsConnection`.
- [ ] Line check every 30 s; no answer → disconnect. A closed socket is noticed at once (unlike Python).
- [ ] Unknown or unexpected packet for the stage → one `warn`, connection dropped (logging policy).
- [ ] One writer per connection, whole frames only (Python's interleaved writes must not exist here).

### OPN
- [ ] Memory checks and the patch policy (D3); connection data; auth token accepted (the Nintendo login is AltWFC's or
      Wiimmfi's job); terms and sub-terms with chunking; announce; no-charge; vulgarity; the common-key skip;
      `AnsLmpConnect` with the configured external address.
- [ ] Refusal path (D4): not ready, unsupported disc, NAT hostname not accepted; texts from the database (D1, D6).

### Maintenance mode
- [ ] Standalone (D4): OPN with Wii TLS, every console refused with the message given as an argument,
      `/health/live`; no database, Valkey or file.

### LMP
- [ ] Support codes: read from `AnsConnection`, or issued in `AnsLoginInfo` for a first-time console.
- [ ] PAT tickets in Valkey with a TTL (`AnsTicket`); the next connection finds its session by ticket.
- [ ] The 6 Capcom ID slots (`UserList*`), `AnsUserObject` (existing Capcom ID, or a new one for an empty slot).
- [ ] Server time.
- [ ] World server list (`FmpList*`) with populations read from Valkey (nothing writes them before M2, so 0), and
      `AnsFmpInfo` with the address of the node owning the chosen world server: ownership leases in Valkey start
      here (one node owns them all until the fleet), since `AnsFmpInfo` needs them.

### Config
- [ ] `external_address`: the address handed to consoles in `AnsLmpConnect` / `AnsFmpInfo` (never guessed).

### Database
- [ ] `migrations/0001_schema.sql` (see Schema policy).
- [ ] `scripts/dev.sh --reset`: drops the dev database and Valkey data, for schema changes.
- [ ] Tests use a fresh database each run (`#[sqlx::test]`), so a rewritten schema file never trips them.

### Corpus and harness
- [ ] Replay gate per role: from M1, the OPN and LMP connections of every scenario must replay cleanly, whatever the
      scenario's milestone (FMP connections join in M2).
- [ ] Masks for values that depend on the environment, not only on the run: the LMP and FMP addresses in
      `AnsLmpConnect` / `AnsFmpInfo` are length-prefixed strings whose length can change, so masks learn a
      variable-length kind (`lp2_string` at an offset).
- [ ] `corpus/divergences.toml` starts here (our own texts, patch policy, generated ids), with the harness accepting
      declared divergences only.

## Tests
- [ ] Tier 1: typed packet round-trips on every corpus payload of each type.
- [ ] Tier 2: the replay gate above, in CI, against the in-process server.
- [ ] Tier 3: scripted logins with the test client: a first-time console gets a support code and a Capcom ID; a
      returning console finds its hunters again (also after a server restart); two consoles get different Capcom IDs;
      a stale or unknown ticket is refused; a malformed packet drops the connection with one warn.
- [ ] Tier 4: `.woodpecker/nightly.yml` (cron): the pinned Python MH3SP started in the job, the corpus replayed against
      it with `--nudge --retries`.
- [ ] Tier 6 spike: can Dolphin be driven unattended on the Mac runner (input automation, reading the game's error
      code from memory with the PAL symbol maps)? Build it or drop it, with the reason.

## Logging (info level, anonymous)
Console reached OPN (region), terms declined / accepted is not visible to the server, login to LMP, support code
issued (hashed), Capcom ID created or chosen (`capcom_id` recorded in the span), world server picked. Refused logins
(not ready, patch policy) at `warn`.

## Schema policy (before 1.0.0)
Until the first release, the database schema is one file, `migrations/0001_schema.sql`, rewritten in place. sqlx
refuses to start on a migration whose checksum changed, so any schema change means resetting the database: dev
(`scripts/dev.sh --reset`), CI (fresh services every run) and beta deployments (accounts lost; acceptable during
the beta). From 1.0.0 on, migrations only go forward: new files, never edited, no down scripts.
