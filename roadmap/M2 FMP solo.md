# M2 — FMP, solo

**Status: draft (2026-10-08), decisions to settle.**

From the FMP connection on: the console downloads the game's content, shows the world server list, enters a world
server, a gate, creates a city, creates a quest and starts it, alone. Other players (seeing each other, joining a
city or a quest) are M3.

## Exit criteria

1. A Dolphin console goes from the title screen to a gate of Valor1, creates a city, creates a quest and starts
   it, on the Rust server.
2. The solo scenarios replay cleanly against the Rust server: `s00`, `s02-login-gate` (and `-2`),
   `s02-first-login-gate`, `s03-city-quest-solo` (every connection, FMP included).
3. Tier 3: scripted solo scenarios (gate, city, quest, content downloads) pass.

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

### Open (recommendation first)

- **D1 Content binaries.** MH3SP builds them in Python: server type list, time events, the current event slot,
  trading post, 10 quest slots, per language (types `0x01`–`0x0f` English, then French, German, …).
  - *Recommended:* port the generators to Rust, typed (the quest layout is the big one, `quest_utils.py`);
    English only (as M1's D6); the bytes checked against the recorded downloads.
  - Alternative: embed MH3SP's produced bytes as opaque blobs. Less code now, but event quests and rotation
    (D2) need the generator anyway.
- **D2 Event content** (event quests, rotation schedule, trading post). The global roadmap says "in the database
  from the start, edited from the website in M8".
  - *Recommended:* tables `event_quests` (definition as JSON), `quest_rotation`, `trading_post`, seeded once by
    `mh3sp import-events <MH3SP event folder>`; the server keeps the current rotation in memory and bumps the
    binary version when it changes.
  - Alternative: code constants until M8 (YAGNI), the database only when the website edits them.
- **D3 World model.** 12 world servers × 40 gates × up to 40 cities; capacities 2000 / 100 / 4. Per decision 4,
  a fixed-shape struct behind a `std::sync::Mutex`.
  - *Recommended:* one `World` struct (servers, gates, cities, circles, populations); cities created on
    `LayerCreate` and removed when the last player leaves.
- **D4 Players registry.** Decision 4 plans `DashMap<CapcomId, Player>` (frame channel + public info).
  - *Recommended:* introduce it now, with the `Effect` enum: the solo scenario already gets notifications sent
    to "everyone in the city" (`NtcCircleListLayerChange`), which is the player itself, through the same path
    M3 uses. A second login with the same Capcom ID kicks the first.
- **D5 The hunter profile block** (256 bytes, `UserBinarySet`): in memory with the player (M3 forwards it,
  persists its parsed fields). `NtcCheatCheck`: logged at `trace` and ignored until M6.
- **D6 Time.** Server time, day/night, sandstorm, Jhen Mohran: MH3SP's `time_utils.py` ported as pure functions
  of the clock.
- **D7 Content languages.** The console asks the English set only in every recording (PAL console set to
  English). *Recommended:* English set only; other languages when a console asks (the type tells which).

## Work (once the decisions are settled)

- [ ] FMP packets typed (the 87 types above), round-tripped on the corpus.
- [ ] FMP connection 1: content downloads, world server list (populations from the world), pick.
- [ ] FMP connection 2: gates, entering a gate, city list and creation, layer user lists.
- [ ] Circles (quests): create, match options, info, info set, match start/end, leave.
- [ ] Content binaries (D1), event content (D2), time (D6).
- [ ] World model (D3), players registry and `Effect`s (D4).
- [ ] Replay gate: the solo scenarios, every connection.
- [ ] Tier 3 solo scenarios.
- [ ] Dolphin run (exit criterion 1).
