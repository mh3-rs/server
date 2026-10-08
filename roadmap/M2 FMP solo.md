# M2 — FMP, solo

**Status: decisions settled (2026-10-09), ready to start.**

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

### Settled (2026-10-09)

- **D1 Content binaries**: MH3SP's generators ported to Rust, typed (server type list, time events, current event
  slot, trading post, quest slots; the quest layout from `quest_utils.py`), checked against the recorded downloads.
- **D2 Event content** (event quests, rotation, trading post): **files, as MH3SP has them** (`event/*.json`,
  `quest_rotation.json`, `trading_post.json`, same format), read at startup from a content folder (`CONTENT_DIR`,
  default `content/`, in the repository and the image). Nothing hard-coded, no database. A change takes a
  restart (a reload, or the website editing them, decided when needed).
- **D3 World model**: one `World` struct behind a `std::sync::Mutex` (decision 4): 12 world servers × 40 gates ×
  up to 40 cities, capacities 2000 / 100 / 4. **Resources exist only on demand**: a gate, a city or a circle is
  created when the first player enters (or creates) it and removed when the last one leaves; lists show the rest
  as empty.
- **D4 Players registry**: `DashMap<CapcomId, Player>` (frame channel + public info) and the `Effect` enum, now:
  solo play already gets notifications sent "to the city", which is the player itself, through the path M3 uses.
  A second login with the same Capcom ID kicks the first.
- **D5 Hunter profile block** (256 bytes, `UserBinarySet`): in memory with the player (M3 forwards it and
  persists its parsed fields). `NtcCheatCheck`: logged at `trace`, ignored until M6.
- **D6 Time**: MH3SP's `time_utils.py` ported as pure functions of the clock (server time, day/night, sandstorm,
  Jhen Mohran).
- **D7 Content languages**: the English set (`0x01`–`0x0f`, all the recordings ask); other languages when a console
  asks for them.

## Work

- [ ] FMP packets typed (the 87 types above), round-tripped on the corpus.
- [ ] FMP connection 1: content downloads, world server list (populations from the world), pick.
- [ ] FMP connection 2: gates, entering a gate, city list and creation, layer user lists.
- [ ] Circles (quests): create, match options, info, info set, match start/end, leave.
- [ ] Content binaries (D1); the content folder with MH3SP's event files (D2); time (D6).
- [ ] World model (D3), players registry and `Effect`s (D4).
- [ ] Replay gate: the solo scenarios, every connection.
- [ ] Tier 3 solo scenarios.
- [ ] Dolphin run (exit criterion 1).
