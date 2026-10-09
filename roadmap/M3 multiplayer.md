# M3 — Multiplayer

**Status: decisions settled (2026-10-09), all tested on consoles.**

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
  the game's **message inbox**, answered from there later, but only until the game quits (lab, 2026-10-09) (`ReqFriendAccept`); both get `NtcFriendAdd`
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
- **D3 The hunter profile block. Settled, done:** first planned in stages (raw block plus the known fields, fields
  moving out of raw as they got mapped); then the game's code mapped every byte, so the block is parsed whole,
  validated, stored as fields (`hunter_profiles`, `hunter_equipment`) and rebuilt from them; no raw column.
- **D4 Disconnects. Settled (lab, 2026-10-09): no grace period.** A closed connection is a departure, as before.
  Tested with `lab/cut.sh` (pf blocks one Dolphin's sockets for N seconds), two consoles:
  - The **server link** survives cuts of 5, 15 and 20 s (TCP resends what was held back; nobody notices). The game
    gives up on its own after 20–30 s without the server ("network instability", back to the title, a new login);
    the server never dropped anyone first (its line check drops only after 30 s unanswered, then 30 more).
  - The **P2P link** between consoles holds 15 s, not 20: mid-quest, both see "dev exited the game" and hunt on
    alone; the cut console then reports `0x8003003f` (11674), leaves the party and returns to town, where joining
    the city host's P2P session (the host still in its copy of the quest) fails again (11674, back to the gate).
    The consoles sort it out themselves; the server gets the usual leave messages.
  - So a grace period would never help: the server never drops a console the game itself keeps, and the game
    doesn't reconnect once it gives up. For M4: our NatNeg relay, if it carries hunt traffic, should tolerate at
    least the same ~15 s.
  - The server gone mid-quest (checked in M5's lab run 15): the party breaks (the leader sees the other leave at
    once, the other sees the leader frozen), and the way back to town needs the server: a network error on both
    (11674, network instability). A hunt doesn't survive a server stop (M5 D4).
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
- **D7 Leaders and parties. Settled (lab, 2026-10-09): MH3SP's rules; no kick for players in the game (lab
  checks below).** MH3SP's rules. A city's "leader" is the
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
- [x] Chat (MH3SP's colour by level), tells, friends (mutual, pending requests delivered at each game session until answered), black list
      (personal; blocked hunters' tells and requests not delivered) (D1, D2).
- [x] Profile block (D3), the whole block at once instead of stages: every byte mapped from the game's code (its
      fill and sanitiser functions); every write validated against the game's rules (HR, appearance bounds, weapon
      type agreeing with the weapon, equipment kinds per slot and item ids within the game's tables, the comment's
      NUL…), an invalid one refused (previous kept, `warn`); the block others get built from the fields;
      `hunter_profiles` and `hunter_equipment` store them. A profile notice past the block is refused (the receiving
      console copies it at its offset unchecked). Every recorded profile is accepted and rebuilt byte for byte (all
      recordings use the default hunter: appearance and equipment variety untested).
- [x] Disconnect lab test and grace period decision (D4): no grace period.
- [x] Concurrency model written down (who owns what; no socket written but by its task) (D5): `lib.rs`.
- [x] Replay gate: every multiplayer scenario recorded so far replays whole (`REACHED = "M3"`, was `clean_so_far`: `s04`, `s04-2`, `s05`,
      `s06`, `s07`, `s08`, `s09`). The harness accepts a console's own answer before frames recorded ahead of it (the
      Rust server answers at once; MH3SP was slow), and consoles still wait for every frame recorded before their
      next one.
- [x] Tier 3, relay latency (`relay_latency`): 4 consoles in one city, 50 rounds of positions from each, timed from
      send to each receiver (600 deliveries): p99 must stay under 2 ms. Measured (debug build, one Mac): median
      ~0.13 ms, p99 ~0.3 ms, max ~0.6 ms; MH3SP: 50–80 ms.
- [x] Tier 3: 2–4 consoles, disconnect at every step, stress test. `a_disconnect_at_every_step_leaves_no_trace`:
      two consoles meeting (gate, city, quest, ready, departure), either vanishing after any step; the other sees
      them go and keeps playing, then a newcomer finds nothing left. `random_play_leaves_no_trace`: 8 consoles,
      seeded random actions (`SEED=` replays one), vanishing and coming back; the server stays up and nothing is left.
      Found: a ready toggle or party list request racing the quest's end dropped the console; now answered plainly.
- [x] Dolphin run with two consoles (exit criterion 1), 2026-10-09: both in City 1, in one quest, hunting together
      (P2P through AltWFC), the leader abandoning (the other stays, sees "left").
- Lab checks, 2026-10-09 (Rust server, two consoles):
  - [x] Password quest with the `****` placeholder (D9): a wrong password refused ("Wrong Password!"), the right
        one joins. The console never needs the real password.
  - [x] Kicks (D7): none in the game, for players: not in the menus, in a city or a quest party, nor in Local Info
        once it showed the real population (run 8); nothing online either; the protocol has no city kick (only
        quest party kick packets, never offered in the menus). The only removal is at departure (players not ready
        stay behind). Kicks from the website: a future step (global roadmap).
  - [x] Messages and friend requests (D2), runs 2–5: the game's inbox doesn't outlive the game, drops a message
        arriving while a scene loads (sent at the world server, or on entering a gate: lost), and deleting from it
        is local (no packet). A plain message is `ReqTell` → `NtcTell` (what the corpus calls a tell: the same
        inbox message), a request `ReqFriendAdd` → `NtcFriendAccept`. Settled: both are stored (`messages`) and
        delivered once, when the recipient stands in a city (at once if they do, else at their next position
        there; confirmed runs 5–6: messages sent while the recipient was at the gate selection or in a gate
        all arrived on entering a city, then were gone from the database; crossed requests made friends); a plain message is then deleted, a request kept 30 days as the proof an answer
        needs; sending it again delivers it again; two hunters asking each other become friends at once. A blocked
        sender's messages are never stored; 100 waiting per recipient at most; everything expires after 30 days. A
        friend deletion ends it for both; the other's list updates at its next login (no notice in the protocol).
  - [x] Found: picking a used slot, the console sends no name when the slot list shows its hunter's; the server
        took that as an empty name: stored it, wiped the hunter's profile, friends and blocks (an empty "from" on
        the friend request, a nameless hunter in town). Fixed: no name keeps the stored one.
  - [x] Local Info's city population said 0/4. Found in the game's code: the screen shows the console's own layer
        record (population `0x06`, capacity `0x09`, then the city's settings), which only `AnsLayerStart`,
        `AnsLayerChildInfo` *echoing the request's 0* and `NtcLayerUserNum` (kind 2) write; MH3SP (and we) answered
        1, so the console kept the login placeholder (0 of 4). Fixed: the request's number echoed, the city's
        settings sent, `NtcLayerUserNum` to the city on every join and leave. Confirmed (run 8): 1/4 alone, 2/4 live
        when the other joins, back live when they leave; the city's setting ("casual play") shown. No player list
        and no kick on that screen.
  - [x] The disconnect test (D4): see D4, settled.
