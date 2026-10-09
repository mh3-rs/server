# M2 — FMP, solo

**Status: done (2026-10-09).**

From the FMP connection on: the console downloads the game's content, shows the world server list, enters a world
server, a gate, creates a city, creates a quest and starts it, alone. Other players (seeing each other, joining a
city or a quest) are M3.

## Exit criteria

1. ✓ A Dolphin console goes from the title screen to a gate of Valor1, creates a city, creates a quest and starts
   it, on the Rust server (2026-10-09, about 55 s of play from OPN to leaving the quest). Server answers over the
   session: median 0.1 ms, slowest 4.4 ms (quest binaries built on demand, debug build); the waits are the game's
   own and Dolphin's.
2. ✓ The solo scenarios replay cleanly against the Rust server: `s00`, `s02-login-gate` (and `-2`),
   `s02-first-login-gate`, `s03-city-quest-solo` (every connection, FMP included).
3. ✓ Tier 3: scripted solo scenarios (gate, city, quest, content downloads) pass.

## Protocol: what the console does

FMP (port 8202) comes as **two connections**, each opened with the ticket of the last LMP visit
(`NtcLogin` status 3):

**1. Server selection.** Content first, then the world server list (the screen the player sees), the pick:
```
→ ServerTime → BinaryHead/Data/Foot (type 1: server types) → UserSearchInfoMine → LayerStart → CircleInfoNoticeSet
→ NtcCheatCheck → UserBinarySet → UserSearchSet → BinaryVersion + Binary* (time events, …)
→ FmpListVersion2 / FmpListHead2 / FmpListData2 / FmpListFoot2 → FmpInfo2 (the pick) → LayerEnd → Shut
```
**2. The world server.** The same start, then the gates, a gate, a city, a quest:
```
→ … LayerStart → LayerUserList → … BinaryVersion + Binary* ×3 → FriendList → BlackList → UserStatusSet
→ LayerChildListHead / Data ×4 / Foot (the 40 gates) → LayerChildInfo → LayerDown (enter the gate)
→ LayerChildInfo → LayerUserList → UserBinarySet → UserSearchSet → UserStatusSet → UserBinaryNotice
→ LayerChildList* (the cities) → LayerCreateHead / Set / Foot (create a city) → LayerChildInfo → LayerUserList
→ BinaryVersion + Binary* ×9 (quests, trading post, …) → LayerMediationList → CircleListLayer
→ CircleCreate → CircleMatchOptionSet → CircleInfo → CircleInfoSet → CircleMatchStart (quest starts:
  NtcCircleMatchStart) → NtcCircleBinary → CircleMatchEnd → CircleLeave → … → LayerEnd → Shut
```
87 packet types on FMP in the solo scenarios. Content binaries asked: types `0x01`–`0x0f` (the English set),
from 12 bytes (time events) to 5 KB (server types); `0x06`–`0x0f` are the 10 quest slots.

Reference: MH3SP `fmp_server.py`, `mh/pat.py`, `mh/database.py` (world model), `mh/constants.py` (`PAT_BINARIES`),
`mh/quest_utils.py`, `mh/time_utils.py`, `event/*.json`; MHTrIDA for the layouts.

## Decisions

### Settled (2026-10-09)

- **D1 Content binaries**: MH3SP's generators ported to Rust, typed (server type list, time events, current event
  slot, trading post, quest slots; the quest layout from `quest_utils.py`), checked against the recorded downloads.
- **D2 Event content** (event quests, rotation, trading post): **files, as MH3SP has them** (`event/*.json`,
  `quest_rotation.json`, `trading_post.json`, same format), in a content folder (`CONTENT_DIR`, default
  `content/`). Nothing hard-coded, no database. **Content never goes in the code repository or the image**: a
  deployment mounts its folder (a volume); dev, tests and CI point `CONTENT_DIR` at an MH3SP checkout at the
  pinned commit (`f53b06c`, the one the corpus was recorded with). **Read on demand**: a binary is
  built from the files when a console asks for it (`ReqBinaryHead`) and kept for that download, so its chunks
  and foot come from one snapshot. Its version (`AnsBinaryVersion`) is derived from its bytes: an edited file,
  or the rotation moving on, changes it, and connected consoles fetch the new content at their next poll. No
  restart, no reload command. Every file is also checked at startup: errors logged, not fatal (one broken quest
  doesn't stop the server; the download that needs it fails with an `error` naming the file).
- **D3 World model**: one `World` struct behind a `std::sync::Mutex` (decision 4): 12 world servers × 40 gates ×
  up to 40 cities, capacities 2000 / 100 / 4. **Resources exist only on demand**: a gate, a city or a circle is
  created when the first player enters (or creates) it and removed when the last one leaves; lists show the rest
  as empty.
- **D4 Players registry**: `DashMap<CapcomId, Player>` (frame channel + public info) and the `Effect` enum, now:
  solo play already gets notifications sent "to the city", which is the player itself, through the path M3 uses.
  A second login with the same Capcom ID kicks the first.
- **D5 Hunter profile block** (256 bytes, `UserBinarySet`): in memory with the player (M3 forwards it and
  persists its parsed fields). `NtcCheatCheck`: logged at `trace`, ignored until M9 (anti-cheat).
- **D6 Time**: MH3SP's `time_utils.py` ported as pure functions of the clock (server time, day/night, sandstorm,
  Jhen Mohran).
- **D7 Content languages**: the English set (`0x01`–`0x0f`, all the recordings ask); other languages when a console
  asks for them.

## Work

- [x] FMP packets typed: all 107 types the corpus has (M3's multiplayer ones too), each recorded payload decoded
      whole and re-encoded byte for byte. New field shapes: optional fields, records with optional fields, a
      u8-counted record list, and `Rest` (opaque trailing bytes). Messages relaying game data
      (`NtcLayerBinary`, `NtcCircleBinary`, `NtcLayerUserPosition`, …) have a layout per direction: their common
      prefix is typed, the rest kept opaque (so only that prefix is checked).
- [x] FMP lifecycle: both FMP connections take the session by the ticket of the last LMP visit (`NtcLogin` 3)
      and give it back under the same ticket at `ReqShut` (FMP issues none). Players registry
      (`Players`, a `DashMap` by Capcom ID) with a mailbox per connection; a second login of the same Capcom ID
      kicks the first; a disconnect unregisters only its own entry. The mailbox's frame channel comes with the
      first notification sent to someone else. Scripted test: `fmp_sessions_hop_and_kick`.
- [x] FMP connection 1 (server selection): content downloads (one snapshot per download, versions from the
      bytes; binaries `0x01` server types, `0x05` MH3SP's placeholder: its purpose is unknown, the game fetches it
      as file 5), `LayerStart`, search info (two numbers derived from the Capcom ID), the profile block (in the
      session), the world server list and the pick. Every server selection connection of the corpus replays
      cleanly (17, `s00` whole), now in the replay gate. Divergences: content versions; populations (0 until the
      world; MH3SP's ghost on Greed4: its LMP joins the servers the console only asks the address of).
- [x] FMP connection 2, up to the gates: the world (`World`: placed players only, with world server, gate, name
      and profile; populations and lists counted from them, D3), the player list, friend and black lists (MH3SP's
      blank entry kept), user status, the 40 gates, entering a gate, leaving. Content binaries `0x02` (Jhen Mohran
      times), `0x03` (NPC greeters: quest names from the rotation's files), `0x04` (trading post), read from
      `CONTENT_DIR` (tests and CI: MH3SP at `f53b06c`, cloned by `scripts/ci-tests.sh` when absent). Replays run
      at the scenario's recording date (`clock_offset`). The three `s02` scenarios replay whole (`clean_so_far`).
- [x] FMP connection 2, cities: the city list of a gate (with each city's settings), creating one
      (`LayerCreateHead` reserves it, shown full; `Set` creates it and puts its creator in; `Foot` ends the
      reservation), entering and leaving one. A city exists from its reservation until its last player leaves; the
      reservation goes with its player; a leader leaving hands the city to the earliest player left. Refusals are
      alerts (the answer's id with kind byte `0x01`). Layer states are MH3SP's: 1 empty, 2 full, 3 joinable. `s03`
      creates its city and stops at the quest downloads.
- [x] Circles (quests): mediation list (MH3SP's placeholder), circle list, create, match options, info, info set
      (relayed, not applied: MH3SP's behaviour), match start (the players ready depart), match end, leave, game
      data relayed to the circle. A circle lives from its creation until its leader leaves. Notifications: the
      requester's own copy goes in its answer, in the recorded order; others' copies to their mailboxes (a bounded
      channel per connection; a full one drops its connection). No `Effect` type needed.
- [x] Content binaries (D1), from MH3SP's event files (D2), time (D6): all of `0x01`–`0x0f`. Quests are parsed
      into typed structs (`content/quest.rs`) and laid out as MH3SP does; checked once against MH3SP's own
      generator on all 40 quest files: byte-identical. MH3SP quirk kept: Grudge Match Wyvern Trio and Land Lords
      have 9 items in their last arena set's 8-slot pouch, written whole.
- [x] World model (D3), players registry and mailboxes (D4).
- [x] Replay gate: the solo scenarios, every connection (`REACHED = M2`: `s00`, the three `s02`, `s03`).
- [x] Tier 3 solo scenarios (`crates/testkit/tests/server.rs`): `solo_city_and_quest` (gate and city lists,
      reservation, the creator's own notifications before its answers, departure, leaving), `cities_are_shared`
      (a reserved city shown full and refused to another player with an alert, then joined; the city survives its
      creator leaving), `content_downloads` (every type `0x01`–`0x0f`: version, head and chunks agree).
- [x] Dolphin run (exit criterion 1).
