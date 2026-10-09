# M3 — Multiplayer

**Status: draft (2026-10-09), decisions to settle.**

Players see each other: in a gate and a city (arrivals, departures, positions, the game data consoles exchange
through the server), in a quest party (join, ready, depart, leave, leader changes), and socially (chat, tells,
friends, black list). The hunts themselves stay console to console (P2P through AltWFC's NAT negotiation; our own
NAT negotiation and relay are M4).

## Exit criteria

1. Two Dolphin consoles in one city, both in the same quest, on the Rust server (AltWFC doing NAT negotiation).
2. The multiplayer scenarios replay cleanly: `s04-city-join` (and `-2`), `s05-quest-party`, `s06-disconnect`, and
   the new chat and friends recordings (D1).
3. Tier 3 with 2–4 consoles: join/leave, leader transfer, chat, friends, a disconnect at every step, a stress test
   (many consoles, random actions); the relay-latency test (positions through one city, p99 under 2 ms on one host).

## Protocol: what the recordings show

From the corpus (the M3 scenarios; counts over all of them):

- **Entering a city someone is in**: `LayerHost` (the city's host: its path and Capcom ID), `LayerUserListHead` /
  `Data` / `Foot` (the players of a layer, searched by path), then `NtcLayerIn` to the others; leaving:
  `NtcLayerOut`. A city's record in lists carries its host's path (field `0x02`) once occupied.
- **Town relays**, the bulk of the traffic: `NtcLayerUserPosition` (94, positions) and `NtcLayerBinary` (52, game
  data) sent to everyone in the layer; `NtcLayerBinary2` (12) sent to one player: the P2P connection offers that
  let consoles open their direct UDP link. The server prefixes each with the sender's Capcom ID (and name).
- **Quest party**: `CircleJoin` (→ `NtcCircleJoin` to the members), `CircleUserList`, `CircleHost`,
  `NtcCircleMatchOptionSet` (ready), `NtcCircleMatchStart` to everyone, `NtcCircleBinary` / `Binary2` (party data,
  to all or one), `NtcCircleLeave`, `NtcCircleHost` (a new leader after departure), `NtcUserBinaryNotice`
  (a profile change, to the layer).
- **Abrupt disconnect** (`s06`): MH3SP notices on its next line check, ~30 s later, then sends `NtcLayerOut`.

Not in the corpus, to record (D1): layer chat, tells, friend requests (add, accept, delete), the black list, a
player kicked for not being ready, a party broken by its leader before departure.

## Decisions

### Open (recommendation first)

- **D1 New recordings.** Chat, tells, friends and the black list are in no recording.
  *Recommended:* record them with two Dolphins at the start of M3, in the lab, against MH3SP (the oracle):
  `s07-chat` (layer chat, a tell), `s08-friends` (request, accept, list, delete), `s09-blacklist`, `s10-party`
  (the leader kicks a member, then lifts the kick list; a password-protected quest joined with the wrong then the
  right password; start with a player not ready; a leader leaving before departure).
- **D2 Friends and black list storage.** Lists are permanent: SQLite tables `friends` and `blacklist` (pairs of
  hunters, as sketched in `M1 schema.md`). A friend request reaches the recipient as a message (`NtcFriendAccept`,
  built like a tell), answered with `ReqFriendAccept`; the protocol has no "pending requests" list. Whatever the
  console does with it, the server must remember a request until it's answered (else a console could accept a
  request never sent): *recommended:* pending requests in SQLite (`friend_requests`), which also allows delivering
  one to an offline player at their next login (MH3SP loses them). What the game shows decides the rest, to watch
  while recording (D1): if it keeps received requests to answer later, the server just stores and checks; if not,
  the server could tell players about pending requests itself, e.g. a private message from "system".
- **D3 The hunter profile block.** Stage 1 of the global plan: parse and validate the known fields (HR u16 @0,
  weapon type u8 @5) on every `UserBinarySet`; an invalid update is refused (the previous block kept and
  forwarded, one `warn`). *Recommended:* validation now (other consoles draw the hunter from it), the
  `hunter_profiles` table later, with its first reader (the website, M7, or anti-cheat, M6).
- **D4 Disconnects.** The Rust server sees a closed socket at once: others get `NtcLayerOut` immediately, not ~30 s
  later as with MH3SP (a vanished console that sends nothing is still dropped by the line check: 30 s, then one
  unanswered). *Recommended:* keep it; `s06`'s timing is a declared divergence.
- **D5 Locking. Settled:** one lock for the whole `World` (relays look up the layer's players under it and push to
  their mailboxes without waiting); written down where it's declared (`Shared::world`). Revisited the day the
  relay-latency test or production shows contention: per world server first (nothing crosses that boundary but
  population counts), finer locks (gate, city, quest) only with a lock ordering for every move between levels.
- **D6 A slow player.** A full mailbox (256 frames) drops its connection today. Positions alone are ~0.23 kB/s per
  player. *Recommended:* keep (a console that far behind is gone); revisit with the stress test.
- **D7 Leaders and parties**, MH3SP's rules. A city's "leader" is the **host of its P2P session**: a console joining
  asks `LayerHost` who it is and connects to that console directly; the protocol has no kick from a city (to check
  in the game's menus while recording). A **quest's leader can kick** a member: `ReqCircleKick` (→ `NtcCircleKick`
  to the kicked player), and a kick list keeps them out of that quest (`ReqCircleDeleteKickList` lifts it). The host
  leaving hands the session to the earliest player (`NtcLayerHost`), and the others reconnect to them; a quest's leader leaving before departure breaks the party (`NtcCircleBreak`), after departure
  hands it over (`NtcCircleHost`); at departure, players not ready are kicked from the party (`NtcCircleKick`).
  *Recommended:* the same.
- **D9 Quest capacity and password. Settled:** both are set at creation and enforced at `CircleJoin` ("quest is
  full", "wrong password" alerts). The password is **never kept in clear**: the server keeps its HMAC-SHA256 under a
  random key generated at startup (memory only) and compares a join attempt's HMAC to it; it's **dropped at
  departure** (a departed quest can't be joined). Never in the database or the logs. MH3SP puts the password itself
  in the quest's record, sent to everyone in the city (copied in M2); the record also has a "has password" flag.
  To test while recording: does the console only need the flag? If so the password stops leaving the server (it
  must, for the HMAC to replace it).
- **D8 Tier 4** (live differential, kept for later in M1). M3 is where it pays off: scripted multi-console scenarios
  the corpus doesn't have, run against MH3SP and Rust. *Recommended:* decide after D1's recordings (they may cover
  enough).

## Work (once the decisions are settled)

- [ ] Recordings (D1), imported, masks and divergences.
- [ ] Layers: `LayerHost`, `LayerUserList*` (search by path), `NtcLayerIn` / `Out`, a city's host path in lists,
      `NtcLayerHost`; town relays (`NtcLayerUserPosition`, `NtcLayerBinary`, `NtcLayerBinary2`).
- [ ] Parties: join, user list, host, binaries to all / one, leave, kick, break, host transfer (D7);
      `NtcUserBinaryNotice`.
- [ ] Chat, tells, friends, black list (D1, D2).
- [ ] Profile block validation (D3).
- [ ] Concurrency model written down (who owns what; no socket written but by its task) (D5).
- [ ] Replay gate: the multiplayer scenarios.
- [ ] Tier 3: 2–4 consoles, disconnect at every step, stress test, relay latency.
- [ ] Dolphin run with two consoles (exit criterion 1).
