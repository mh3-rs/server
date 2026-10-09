# M3 — Multiplayer

**Status: decisions mostly settled (2026-10-09); open until tested on consoles: D4 (disconnects), D7 (kicks and
leaders), D9's password behaviour.**

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

- **D1 New recordings. Settled:** record with two Dolphins at the start of M3, in the lab, against MH3SP (the
  oracle): `s07-chat` (layer chat, a tell), `s08-friends` (request, accept, list, delete; does the console keep a
  received request to answer later?), `s09-blacklist`, `s10-party` (the leader kicks a member, then lifts the kick
  list; a password-protected quest joined with the wrong then the right password; start with a player not ready; a
  leader leaving before departure). Also look for a city kick in the game's menus.
- **D2 Friends and black list storage. Settled:** lists are permanent: SQLite tables `friends` and `blacklist`
  (pairs of hunters, as sketched in `M1 schema.md`). Seen on consoles (`s08-friends`): a friend request is an
  option of a message (`ReqFriendAdd` with a text); the recipient gets it as a message (`NtcFriendAccept`) kept in
  the game's **message inbox**, answered from there any time later (`ReqFriendAccept`); both get `NtcFriendAdd`
  and their lists update at once. So the server stores pending requests (SQLite `friend_requests`) until answered,
  and refuses an answer to a request never sent; a request to an offline player waits for their next login. The
  friend list is only asked for when entering a world server, and the protocol has no notification of a deletion:
  the other player's list updates at its next login. A friendship is **mutual**: one row per pair,
  `friends (hunter_a, hunter_b)` referencing `hunters` (`ON DELETE CASCADE`), `CHECK (hunter_a < hunter_b)` so a pair
  has one order, primary key on the pair; a deletion by either side removes the row, so the friendship ends for both
  (MH3SP removes it from the deleter's list only). A hunter replaced on its slot drops its friendships (M1 schema).
  **Open:** whether to tell the other player with a message from "system".
  **The black list is personal** (`s09-blacklist`): `blacklist (blocker, blocked)` referencing `hunters`,
  `UNIQUE (blocker, blocked)` ((a, b) and (b, a) are separate rows), only the blocker removes their row. The game
  offers blocking from a received message, keeps its own copy of the list (it refuses a duplicate itself), and
  filters nothing: the server refuses to deliver to a blocker what their blocked hunters send them (tells, messages,
  friend requests); city chat stays visible (they're in the same city). MH3SP's blank placeholder entry in every
  black list shows on the console as an empty line it can't delete: the Rust server sends an empty list (it copied
  the placeholder in M2).
- **D3 The hunter profile block. Settled:** stored in the database (`hunter_profiles`), as the raw block *and* the
  known fields, parsed and validated (HR u16 @0, weapon type u8 @5 so far; an invalid update is refused: the previous
  block kept, one `warn`). The block other consoles receive is rebuilt: the raw bytes with the known fields written
  over them. As fields get mapped they move from raw to parsed; once all are, the raw column goes (global roadmap,
  M3).
- **D4 Disconnects. Open, a lab test first.** The Rust server sees a closed socket at once (MH3SP ~30 s later, at
  its next line check). A grace period (keeping a vanished player's place for a few seconds) only helps if the game
  can come back: in the lab a broken connection so far ends on an error screen and back to the title, a new login;
  but hunts are P2P, and telling the others "left" mid-quest might drop a player whose server connection only
  hiccuped. The protocol has an `NtcRecconect` (sic) message: the game may support some reconnection. Test: cut one
  Dolphin's network for 5, 15, 30 s, in town and mid-quest, watch both consoles; then decide.
- **D5 Locking. Settled:** one lock for the whole `World` (relays look up the layer's players under it and push to
  their mailboxes without waiting); written down where it's declared (`Shared::world`). Revisited the day the
  relay-latency test or production shows contention: per world server first (nothing crosses that boundary but
  population counts), finer locks (gate, city, quest) only with a lock ordering for every move between levels.
- **D6 A stuck player. Settled:** a full mailbox (256 frames) disconnects its player. It's a memory guard, not a
  latency rule: a mailbox holds frames only until the connection's task writes them to its socket, into the
  system's TCP send buffer (128 KB or more). Measured on the recordings, a player receives under 1 frame/s (9 at
  the busiest second), ~80 bytes each; a full city, ~3/s with bursts around 30. A slow console just reads late
  from the system buffer; the mailbox fills only after ~1,600 + 256 frames unread, about a minute of not reading at
  a full city's busy rate (the line check usually drops it first). Others aren't slowed: senders never wait.
- **D7 Leaders and parties. Open until tested on a console (D1).** MH3SP's rules. A city's "leader" is the
  **host of its P2P session**: a console joining asks `LayerHost` who it is and connects to that console directly;
  the protocol has no kick from a city (to check in the game's menus). A **quest's leader can kick** a member:
  `ReqCircleKick` (→ `NtcCircleKick` to the kicked player), and a kick list keeps them out of that quest
  (`ReqCircleDeleteKickList` lifts it). The host leaving hands the city's session to the earliest player
  (`NtcLayerHost`), and the others reconnect to them; a quest's leader leaving before departure breaks the party
  (`NtcCircleBreak`), after departure hands it over (`NtcCircleHost`); at departure, players not ready are kicked
  from the party (`NtcCircleKick`).
- **D9 Quest capacity and password. Settled:** both are set at creation and enforced at `CircleJoin` ("quest is
  full", "wrong password" alerts). **The server checks the password**: a joining console sends the one typed in
  `ReqCircleJoin`. It's **never kept in clear**: hashed with argon2 at creation (off the async threads: tens of
  milliseconds, once per creation and per join attempt), compared at each join, **dropped at departure** (a departed
  quest can't be joined). Never in the database or the logs. MH3SP also puts the password itself in the quest's
  record (field `0x04`), sent to everyone in the city with the quest list, every change and every info request;
  copied in M2. **The password never leaves the server**: the record has a "has password" flag; to test while
  recording that the console only needs the flag; if it needs field `0x04` present too, the server sends a fixed
  placeholder instead of the password.
- **D8 Tier 4. Kept for later** (live differential against MH3SP).

## Work (once the decisions are settled)

- [x] Recordings (D1): `s07-chat`, `s08-friends`, `s09-blacklist` recorded and imported; `s10-party` to record.
      Divergences: the black list's blank entry gone (a mask to the end of the payload, back in the harness); a
      blocked hunter's tell not delivered (`[[omit]]`, new: frames the Rust server leaves out on purpose).
- [x] Layers: `LayerHost`, `LayerUserList*` (search by path), `NtcLayerIn` / `Out` (at gate and city level; a
      disconnect too; a kicked old connection notifies nobody), a city's host path in lists, `NtcLayerHost`; town
      relays (`NtcLayerUserPosition`, `NtcLayerBinary`, `NtcLayerBinary2`: the console's empty sender record replaced
      by the real one); player search (`LayerUserSearch*`), player cards (`UserSearchInfo`), console error reports
      (`NtcCollectionLog`, logged).
- [x] Parties: join (full, gone, wrong password), user list, host, binaries to all / one, leave, break, host
      transfer (D7); `NtcUserBinaryNotice`; players not ready stay behind at departure (`NtcCircleKick`). `s10`
      showed no kick in the menus against MH3SP (whose Local Info said 0/4 players: maybe the reason). A leader whose
      console vanishes is cleaned up, before and after departure (tier 3).
- [x] Chat (MH3SP's colour by level), tells, friends (mutual, requests stored and delivered once), black list
      (personal; blocked hunters' tells and requests not delivered) (D1, D2).
- [x] Profile block (D3): `game/profile.rs` validates every write (HR ≤ 999; weapon type ≤ 8, a guess: every
      recording has 1), an invalid one refused (previous kept, `warn`); `hunter_profiles` stores the raw block and the
      known fields; others get the rebuilt block. Every recorded profile is accepted and rebuilt byte for byte.
- [ ] Disconnect lab test and grace period decision (D4).
- [x] Concurrency model written down (who owns what; no socket written but by its task) (D5): `lib.rs`.
- [x] Replay gate: every multiplayer scenario recorded so far replays whole (`clean_so_far`: `s04`, `s04-2`, `s05`,
      `s06`, `s07`, `s08`, `s09`). The harness accepts a console's own answer before frames recorded ahead of it (the
      Rust server answers at once; MH3SP was slow), and consoles still wait for every frame recorded before their
      next one.
- [x] Tier 3, relay latency (`relay_latency`): 4 consoles in one city, 50 rounds of positions from each, timed from
      send to each receiver (600 deliveries): p99 must stay under 2 ms. Measured (debug build, one Mac): median
      ~0.13 ms, p99 ~0.3 ms, max ~0.6 ms; MH3SP: 50–80 ms.
- [ ] Tier 3: 2–4 consoles, disconnect at every step, stress test.
- [ ] Dolphin run with two consoles (exit criterion 1).
- [ ] Lab checks: with two consoles on the Rust server, does Local Info now show the real population (the Rust
      server answers `LayerChildInfo` with the real layer, MH3SP a 0/4 placeholder), and does a kick option appear
      then (none found against MH3SP, `s10`)? Does a friend request survive in the inbox across a game restart
      (deliver once vs again)? Does
      a password quest work with the placeholder instead of the password (D9)? A city kick in the menus (D7)?
      The disconnect test (D4).
