# mh3sp-rs roadmap

A from-scratch Rust server for *Monster Hunter Tri* (Wii) online play, replacing
[MH3SP](https://github.com/sepalani/MH3SP) (Python) in one big-bang migration.
Python MH3SP stays the **oracle**: whenever the two disagree on the wire, the
Python behaviour is right until proven otherwise.

## Goals

- Byte-compatible with the real game (PAL `RMHP08` first, then `RMHE08`, `RMHJ08` later).
- One binary, every server role, `tokio` async.
- Protocol layer separated from game logic, so logic is testable without sockets.
- Built-in NAT negotiation + UDP relay (strategies: `p2p` | `relay` | `hybrid`).
- Deployable as a fleet later (state behind an interface).

## Non-goals (for now)

- Nintendo login (NAS) and GameSpy profile servers: keep using AltWFC or Wiimmfi.
- Offline content tooling (`dump_quest.py`, `dump_trading_post.py`): the JSON they produce is consumed as is.
- Python 2 anything.
- The RFP server: the game never connects to it (verified by capture, see Facts).

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

## References

| Source | Use |
|---|---|
| `../mh3sp` (branch `dev`) | The oracle. `mh/pat.py`, `fmp_server.py`, `mh/pat_item.py`, `mh/constants.py`, `mh/database.py`, `mh/quest_utils.py` |
| `../mh3sp/lab/` | Test rig: `server.sh`, `altwfc.sh`, `play.sh <rvz> [profile] [mac]`, `hosts.sh`, `shot.sh` |
| `../mh3sp/lab/mhtrida` ([MHTrIDA](https://github.com/sepalani/MHTrIDA)) | Packet table with JP names, error codes, save format + checksum, quest files, symbol maps, Wireshark dissector |
| `../mh3sp/lab/mhtriserver` ([MHTriServer](https://github.com/InusualZ/MHTriServer)) | C#: one typed class per packet, a model for the Rust structs |
| [mhtri-decomp](https://github.com/InusualZ/mhtri-decomp) | Game decompilation (`src/Network`, `src/DWCi/DWCi_NatNeg.c`): ground truth for client behaviour |
| `../mh3sp/lab/altwfc/gamespy_natneg_server.py` | NAT negotiation protocol (commands 0x00–0x10) |

## Architecture decisions

1. **One binary, roles by listener.** `opn`, `lmp`, `fmp` listeners in one process; a `natneg` UDP service
   alongside. Each listener can be turned on or off in config (fleet-ready).
2. **Three layers, strict direction:**
   - `wire`: framing (`>HHI` size/seq/id header), typed items, `PatData` records, one Rust type per packet.
   - `game`: pure logic, `fn handle(state, session, Request) -> Vec<Effect>` where `Effect` is
     `Reply | SendTo(capcom_id) | Broadcast(scope) | Disconnect`. No sockets, no I/O.
   - `net`: tokio listeners, per-connection task, executes `Effect`s.
3. **Opaque bytes stay opaque.** Unknown fields (`unk_*`, magic constants) are kept as raw values in the
   structs and written back byte for byte. No "cleanups" before the replay harness proves equivalence.
4. **State behind a trait** (`WorldStore`, `AccountStore`). M0–M5: in-process. Later: a central store for the fleet.
   Sessions and PAT tickets are part of that store (in the Python version they live in memory, which is why a
   server restart drops everyone).
5. **Persistence:** SQLite first (accounts: support code + slot → Capcom ID + name; friend lists).
   MySQL only if a deployment needs it.
6. **TLS:** `openssl` crate (`tokio-openssl`) with `@SECLEVEL=0`, legacy protocol versions and
   `AES128-SHA`/`AES256-SHA`/RC4/DES ciphers. `rustls` cannot speak to a real Wii (1024-bit CA, TLS ≤ 1.0, RSA key exchange).
7. **Config:** one TOML file. Includes the relay strategy and the external IP handed out to clients.
8. **Logging:** `tracing`; packet-level logs with names and, behind a flag, full hexdumps in the same format as
   MH3SP `-d`, so the two can be diffed.

## Verification strategy

1. **Corpus.** Record scenarios in the lab against Python MH3SP with `-d` (full hexdumps, both directions):
   login, gate, city create, city join (2 consoles), quest create/join/start/end, chat, friend request,
   event quest download, trading post, disconnects.
   Store them as `corpus/<scenario>/<connection>.jsonl` (one record per packet: direction, id, seq, hex).
2. **Replay harness.** Feeds each recorded connection's client packets to the Rust server and diffs the replies.
   Volatile fields (tickets, support codes, timestamps, random IDs) are masked per packet type.
3. **Round-trip tests.** Every packet type: `decode(encode(x)) == x`, and `encode(decode(bytes)) == bytes` on all corpus packets.
4. **End-to-end.** The lab's Dolphins pointed at the Rust server: the milestone acceptance tests below.

A corpus diff is a finding, not a nuisance: it is reverse-engineering knowledge the port would otherwise lose.

## Milestones

### M0 — Foundations
- [ ] Cargo project, CI (fmt, clippy, test).
- [ ] `wire`: framing codec (tokio `Decoder`/`Encoder`), items (`Byte`, `Word`, `Long`, `LongLong`, `String`,
      `Binary`), `lp_string`/`lp2_string`, `PatData` with field-id maps.
- [ ] Packet catalogue: all 301 IDs with names (from `mh/constants.py` `PAT_NAMES` + MHTrIDA `PAT.MD`), category/kind decoding.
- [ ] Corpus recorder: a script that turns MH3SP `-d` logs into `corpus/*.jsonl`.
- [ ] Record the first corpus (login → gate → city → solo quest; 2-console city join + quest).
- [ ] Replay harness running (everything fails, that's expected).

**Done when:** every corpus packet round-trips through `wire`, and the harness reports per-packet diffs.

### M1 — Login funnel (OPN + LMP)
- [ ] TLS listener (legacy config), plain TCP listeners.
- [ ] Connection lifecycle: ~1.5 s delay then `ReqConnection`, `NtcLogin` status per stage, 30 s `ReqLineCheck`, timeout.
- [ ] OPN: memory check (game ID at `0x80000000`, patch check address per region), auth token, terms/sub-terms,
      announce, no-charge, vulgarity, `CommonKey` dodge, `LmpConnect`, `Shut`.
- [ ] LMP: login info, support code + PAT ticket issuing, user list (6 slots), user object (Capcom ID assignment),
      server time, FMP list + FMP info (per world-server address).
- [ ] Accounts in SQLite.

**Done when:** a Dolphin goes from title screen to the server list on the Rust server; login corpus replays clean.

### M2 — FMP, solo
- [ ] Session hand-off by PAT ticket across connections (FMP staging → FMP real).
- [ ] World model: 12 world servers (Open/Rookie/Expert/Recruiting), 40 gates, 40 cities, circles; capacities, HR limits.
- [ ] Layer packets: start/end, child list/info, down/up, create (head/set/foot), user lists, detail search.
- [ ] User packets: status/binary/search set, binary notice, search info.
- [ ] Content binaries (`PAT_BINARIES`, 75 entries): server type list, NPC greeters, time events, texts per language,
      trading post; event/arena quests from `event/*.json` + `quest_rotation.json`; version polling.
- [ ] Circles: create, match options, info, info set, match start/end, leave.
- [ ] Server time and day/night, sandstorm and Jhen Mohran schedules.

**Done when:** one console creates a city, creates and starts a quest; solo corpus replays clean.

### M3 — Multiplayer
- [ ] Broadcast scopes: layer (gate/city), circle; `SendTo(capcom_id)`.
- [ ] City join: `LayerHost`, `NtcLayerIn/Out`, `NtcLayerUserPosition`, `NtcLayerBinary`, `NtcLayerBinary2` (directed).
- [ ] Circle join/leave/kick/break, host transfer, `NtcCircleBinary(2)`, list-change notifications.
- [ ] Leader transfer on disconnect (city and circle).
- [ ] Chat (layer, tell), friends (add/accept/delete/list), blacklist.
- [ ] Concurrency model written down: which task owns which state, no cross-task socket writes without channels.

**Done when:** two consoles in one city, both in the same quest, with AltWFC still doing NAT negotiation.

### M4 — NAT negotiation + relay
- [ ] NAT negotiation server (UDP 27901): `INIT`/`INITACK`, `CONNECT`/`CONNECT_ACK`, `REPORT`/`REPORT_ACK`,
      `PREINIT`, address check, NAT-type detection (`NATIFY_REQUEST`, `ERTTEST`).
- [ ] Relay: per (session, peer) UDP port allocation, learns each console's public address from its first packet,
      forwards both ways, idle cleanup, per-session limits.
- [ ] Strategy config: `p2p` (hand out peer addresses), `relay` (always hand out relay addresses),
      `hybrid` (P2P first; on `NN_REPORT` failure or `NtcCollectionLog 0x80030042`, mark the pair or player and hand
      out the relay on the next attempt; the game does not retry by itself, the player retries once).
- [ ] Check: does the game use the `IP:port` inside `NtcLayerBinary2`/`NtcCircleBinary2`? If yes, rewrite it in relay mode.
- [ ] Check: quest topology (host-to-all vs mesh; the decomp hints at host + "subhost"). Needs 3 consoles.

**Done when:** the M3 scenario passes in all three strategies with AltWFC's NAT negotiation turned off.

### M5 — Parity and polish
- [ ] Remaining handlers until the full corpus replays clean (target: all 102 client message types MH3SP handles).
- [ ] Arena quests, market event, trading post rotation, broadcast announcements, whitelist
      (see MH3SP branch `event-quest-rotation-full`).
- [ ] Admin interface (CLI or HTTP): online players, kick, ban, announce.
- [ ] Graceful shutdown, metrics.

**Done when:** the full corpus replays clean and a play session of an hour shows no regression against Python.

### M6 — Anti-cheat (research)

What the server can and cannot see:

| Data | Path | Server can check? |
|---|---|---|
| Hunter profile (HR, gear, stats) | `ReqUserBinarySet`, `HunterSettings` on every layer change | **Yes**: sanity rules (HR range, valid item/equipment IDs, possible combinations) |
| Game's own cheat report | `NtcCheatCheck` (`60801000`, "send cheat check data"), sent after every layer change, currently ignored | **Yes, once decoded**: Capcom's built-in anti-cheat signal |
| Game memory | `ReqMemoryCheck` (`60810100`): the server asks for N bytes at an address | **Yes**: hash code regions to detect Gecko/AR codes; random sampling over a session |
| Town positions | `NtcLayerUserPosition`, relayed through the server | **Partly**: speed/teleport checks, once the format is decoded |
| Quest combat (moves, damage, HP) | P2P UDP between consoles | **No** in `p2p` mode. In `relay` mode the traffic passes through us, but GT2 + game messages would need decoding, and validating damage means simulating game rules |
| Quest results / rewards | Mostly local to the save | **Barely**: quest duration from circle start/end, at best |

Plan:
- [ ] Decode `NtcCheatCheck` (decomp + corpus) and log it; then act on it.
- [ ] Memory-check sampling: known-good hashes of code regions per region/version; mismatch → flag.
- [ ] Profile sanity rules.
- [ ] Moderation tooling: flags, bans by support code / Capcom ID.
- [ ] Research only: decode GT2 in relay mode, position/speed heuristics.

Limits: the client is fully under the player's control. A cheat can fake `NtcCheatCheck` and memory-check answers.
The realistic goal is to stop casual cheats (Gecko codes, edited saves) and give moderators evidence, not perfect integrity.

## Traps to remember

- Legacy TLS (see decision 6). Test against real-Wii settings, not only Dolphin.
- Reproduce unknown bytes exactly; MH3SP has many `# TODO: Field??` constants that the game depends on.
- Copy timings: the ~1.5 s connection delay, 30 s line check, client timeouts.
- `str` vs `bytes` is mixed in MH3SP for Capcom IDs and names: decide each field's real type from the corpus.
- The external IP handed out in `AnsLmpConnect` / `AnsFmpInfo` must be reachable by clients (config, not `0.0.0.0` guessing).
- NAT negotiation must see real client addresses: never behind Docker port mapping or a NAT-rewriting load balancer.
- The game never retries P2P by itself: every fallback costs the player one failed attempt.

## Open questions

- Quest P2P topology (host-to-all vs mesh). Lab with 3 consoles + the decomp's `NetworkSessionStable`.
- Whether `NtcLayerBinary2`'s embedded address is used by the game.
- `NtcCheatCheck` payload format.
- What the 35.80.180.25 (AWS us-west-2) connection seen from Dolphin was: a host missing from the redirection list?
- JP version specifics (EC/shop check, paid-account packets).
