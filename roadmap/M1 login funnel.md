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

## Decisions to take first

- [ ] **Schema, and whether to design for the website (M7) now.** Tables needed by M1: console/hunter accounts.
      Questions: support codes stored hashed (they act as credentials); exactly 6 slots per console like the game;
      Capcom ID format (6 characters as recorded, uniqueness, generation); what else to keep per hunter (name,
      created/last-seen dates, for the website); whether a website account owning several consoles is modelled now
      or added later on top.
- [ ] **Texts** (terms, sub-terms, announce, no-charge, vulgarity lists): where they live. PostgreSQL fits "written
      once, edited from the website later"; files are simpler. Our own texts are declared divergences.
- [ ] **Patch check policy.** OPN reads the game id and the NAT negotiation hostname compiled into the disc. Python
      reads both and accepts anything. Ours: accept known region ids (PAL, US first; JP later), and decide what an
      unexpected hostname means (log it, refuse with a message?).
- [ ] **Maintenance / not ready.** How to refuse new logins with the game's own message when `/health/ready` fails
      (MHTrIDA `net_connect_maintenance`, `ReqMaintenance`, `NtcLogin` status values).
- [ ] **World servers**: where their list lives (names, types Open/Rookie/Expert/Recruiting, HR limits, capacity):
      config or database.
- [ ] **The 1.5 s pause** before `ReqConnection`: MH3SP waits without knowing why ("connect errors if too fast").
      Test against Dolphin whether it is needed.

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
- [ ] Memory checks and the patch policy; connection data; auth token accepted (the Nintendo login is AltWFC's or
      Wiimmfi's job); terms and sub-terms with chunking; announce; no-charge; vulgarity; the common-key skip;
      `AnsLmpConnect` with the configured external address.
- [ ] Not ready → refuse new logins with the maintenance message.

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
