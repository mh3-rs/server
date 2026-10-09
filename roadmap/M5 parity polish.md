# M5 — Parity and polish

**Status: decisions settled (2026-10-09).**

Everything MH3SP does that we don't yet, the content and operations a public server needs, and the leftovers of
M1–M4. After M5 the Rust server can replace MH3SP for players (the public release, M6, the website, M7–M8, and anti-cheat, M9, come after).

## Exit criteria

1. Every client packet MH3SP handles is handled (Japanese-only ones aside: the Japanese version is a future step), each
   seen in a recording that replays whole.
2. The content of MH3SP's live server (its event quests and rotation) served, quests byte-identical to its generator.
3. Announcements and a graceful shutdown that warns players; metrics an operator can watch.
4. A play session of an hour on consoles (2–3 players: cities, quests, chat, friends) with no error and nothing
   slower than MH3SP.

## Where we stand (2026-10-09)

- **Packets.** MH3SP handles 79 client packets; we handle 87 (M3's social and profile work went beyond it). 17 of
  MH3SP's we don't, none ever recorded:
  - party chat: `NtcCircleChat` (chat inside a quest party);
  - searches: `ReqUserSearchHead/Data/Foot` (players by criteria), `ReqLayerDetailSearchHead/Data/Foot` (cities by
    criteria);
  - `ReqLayerSiblingListHead/Data/Foot` (the layers next to one's own), `ReqLayerMediationLock/Unlock`,
    `ReqLayerTell` (chat to one player in the layer; the corpus's tells are the inbox's `ReqTell`);
  - Japanese only: `ReqChargeInfo`, `ReqVulgarityInfoHighJAP`, `ReqVulgarityInfoLowJAP`, `ReqVulgarityLow`.
- **Content.** MH3SP's `dev` (our reference, `f53b06c`) has the event and arena rotation; its live branch
  (`event-quest-rotation-full`, 15 commits ahead) adds event quests, a reworked quest generator (no more `.dat` small
  monster files), live rotation updates, broadcast announcements, a whitelist and chat logs.
- **Announcements** (that branch): an `NtcLayerChat` from a made-up sender (`000000`, "[MH3SP]"), the text cut in
  30-character lines, to everyone.
- **Leftovers:**
  - `roadmap/unk.md`: rename the unknown fields; route `NtcUserBinaryNotice` with `target` 1 to its one player;
  - town movement (M4 run 14): positions go through the server at the layer record's interval (field `0x11`,
    500 ms, MH3SP's guess): moves feel laggy;
  - M3 D4: what a console does when its quest ends while the server is gone (seen hunting on: unchecked after);
  - M4: a mixed city on two machines (one console relayed, the others direct).

## The missing packets (research, 2026-10-09)

From the decomp (the network layer down to the wire is decompiled; the menus calling it aren't, so the menu names are
MH3SP's tested docstrings or inferred) and MH3SP:

| Packets | What the player does | MH3SP | Wire and console |
|---|---|---|---|
| `UserSearchHead/Data/Foot` | Player Search by name or Capcom ID (every server) | substring match on online players | `Data` rows: Capcom ID, name, (profile, path read but dropped), optional fields; up to 100 |
| `LayerDetailSearchHead/Data/Foot` | a city search by its four settings (those chosen at creation) | exact match on settings; a bug: every row is the first city | one city per `Data` (the console clamps to 1), with its members; path in item 2 |
| `LayerSiblingListHead/Data/Foot` | Change Gates (Relocate, at a gate's entrance) | the gates, but no path in them: picking one fails | same shape as the child list; item 2 (path) is the jump target |
| `LayerMediationLock/Unlock` | sitting on a city's seats (1–12) and arm wrestling (13–14) | always yes, notices commented out: two players on one seat, no release on disconnect | refusal = an alert answer; notices `NtcLayerMediationLock/Unlock` to the city |
| `LayerTell` | chat to one hunter, in a gate or world-server lobby only (cities go P2P) | answers, echoes to the sender | `NtcLayerTell` to the target; whether the chat menu offers it: unknown |
| `NtcLayerTellLow` | never sent by the console, and ignored when received | — | not implemented |

**Beyond them:** joining from a list (a sibling gate, a city found by search, maybe a player found) goes through
`ReqLayerInfo` (0x6421) and `ReqLayerJump` (0x6410, same world server) or `ReqLayerJumpReady`/`JumpGo` (0x6416/0x6417,
another): neither MH3SP nor we answer them (on MH3SP these flows hang after the list). And the console only jumps to a
path whose first `u32` is its own, which is 4 in every recording, where our paths (and MH3SP's) carry the depth (1–3).

**Settled (2026-10-09):** seats and arm wrestling done properly (who holds which seat, per city; a taken seat refused
with an alert; the others notified; seats freed when their holder leaves); Player Search built; city search and
Change Gates built together with `ReqLayerInfo` and `ReqLayerJump*` as their own step after M5 (a list one can't act
on is worse than none); `LayerTell` only if a recording shows the chat menu offers it; `NtcLayerTellLow` never. The
console sessions to do: `recordings.md`.

## Decisions


- **D1 New recordings. Settled.** Like M3: two (or three) Dolphins against MH3SP, one scenario per missing feature: party chat
  (`s11`), player and city searches (`s12`), whatever brings sibling lists, mediation lock and `LayerTell` (to find:
  the decomp says when the console sends them). A feature the game never sends from its menus isn't implemented.
- **D2 Content: the live branch's files. Settled.** `CONTENT_DIR` pointed at a checkout of MH3SP's `event-quest-rotation-full`
  (content stays out of this repository); our quest generator updated to its rework (no `.dat` files), checked
  byte-identical on all its quests. Live rotation updates: already ours (files read on demand).
- **D3 Announcements. Settled.** As MH3SP sends them (city chat from "[MH3SP]", 30-character lines), to every logged-in player
  (wherever they are: world server, gate, city; in a quest too). Triggered by the operator: `mh3sp announce "text"`,
  which talks to the running server through its health port (a `POST /announce`, internal like `/health`). The
  website (M8) uses the same endpoint. The same path serves "system" messages later (e.g. the friend-removed idea).
- **D4 Stopping. Settled (2026-10-09).** `mh3sp stop 5min` (a human-readable delay: `1h`, `30s`…) asks the
  running server, through its internal health port, to stop: at once a broadcast ("Server stopping in 5 minutes."),
  reminders at fixed steps (1h, 30min, 15min, 5min, 1min, 30s, 10s, 5s, those shorter than the delay), then the
  connections closed and the process exits. **Nothing is refused meanwhile.** A hunt doesn't survive the stop (lab run
  15: the party breaks with the server connection, even on a direct link, and the way back to town needs the server:
  network errors on both consoles), so refusing departures in a stop's last 15 minutes was tried and **dropped** (run
  16): the game shows no alert for a refused departure, the leader's console just waits frozen (until a member's
  un-ready frees it). Since the warnings reach hunters too (as party chat, see the announcements), players decide.
  Maintenance is only a way to start (`mh3sp maintenance`: a placeholder refusing everyone). `mh3sp stop cancel`
  lifts a stop. SIGTERM (`docker stop`: 10 s by default) runs the same sequence with a 5 s warning; a second one
  stops at once.
- **D5 Metrics. Settled:** `/metrics` on the health port, a JSON of the metrics (always); `/metrics/prometheus`
  behind a `prometheus` cargo feature. Two images: `1` (without) and `1-prometheus` (with; also `1.0-prometheus`,
  `1.0.0-prometheus`). Players online (per world server), cities, quests, NatNeg negotiations (direct, relayed,
  failed), relay pairs and traffic, request latency. No database, no history. Anonymous only: counts, never Capcom
  IDs.
- **D6 Chat logs. Settled: not in M5.** Never logged now (user content, M1's logging rule); what to keep, where and
  how long is for moderation (M7–M9).
- **D7 Town position interval. Settled (lab run 17): 500 ms stays** (MH3SP's value, likely the original servers').
  The layer record's field `0x11` is the console's position interval (how often it sends its own position while
  moving; the decomp's `applyLoginRecord`). Tried on two consoles running in a city:

  | Asked | Sent while moving | Traffic per moving console | Seen on the other console |
  |---|---|---|---|
  | 33 ms | ~16/s (the game rounds to ~62 ms steps) | ~750 B/s | drifting, teleporting: worse |
  | 75 ms | ~8/s | ~380 B/s | a bit jumpy |
  | 200 ms | ~4–5/s | ~200–250 B/s | quite bad |
  | 500 ms | ~2/s | ~100 B/s | the best |
  | 2,000 ms | ~0.5/s | ~21 B/s | choppy: a jump every 2 s |

  The delay between a move and its display barely changes with the interval: it comes from the game's own smoothing,
  tuned for 500 ms. A console standing still sends nothing.
- **D8 Tier 4 (live differential against MH3SP). Settled: dropped.** Kept for later twice; the corpus replays
  (tier 2) and console runs covered parity so far, and MH3SP is a moving reference. Revisit only if a divergence
  can't be found otherwise.
- **D9 Player Search leaves out the searcher. Settled (2026-10-09, s12).** MH3SP lists the searcher among the
  results; the console grays that row out, but it is still a useless result and takes a place in the count and the
  pages. Ours filters it out before paging (a divergence on s12's `dev` and `de` searches).

## Work (once the decisions are settled)

- [x] `roadmap/unk.md`: renames (no change on the wire); `NtcUserBinaryNotice` with `target` 1 to its one player
      (`profile_notices_reach_their_target`); the join/leave `state` note in the code.
- [x] Seats and arm wrestling (`LayerMediationLock/Unlock`): one player per seat per city (`City::seats`), a taken
      seat refused (an alert), the city told (`NtcLayerMediationLock/Unlock`), seats freed when their holder leaves;
      `LayerMediationList` lists the real ones (MH3SP's 14 "Test" placeholders: a divergence entry). Test:
      `one_player_per_seat`. To confirm on consoles (`recordings.md`: s13 and the Rust check).
- [x] Player Search (`UserSearchHead/Data/Foot`): every world server, part of a Capcom ID or of a name (any case),
      as MH3SP but without the searcher (D9); nothing without either; the pages the console asks for (MH3SP sent
      everything each time). Rows as MH3SP's `UserSearchInfo` (s12): Capcom ID, name, the hunter's stats block
      (`0x03`), the path (`0x04`); the layer search's rows have the path in `0x03`. Test: `player_search`; s12 replays.
      To confirm on consoles (`recordings.md`: the Rust check).
- [x] Found by s12's replay (2026-10-09):
  - a gate's city list stays live: the players standing in the gate get a city's counts (`NtcLayerUserNum`, kind 4)
    when it's created or someone leaves it, and its whole record (`NtcLayerInfoSet`) when it's reserved, created or
    emptied, as MH3SP (test: `cities_are_shared`);
  - lists show players in the order they reached the layer (MH3SP's), not the world server (`World::arrive`); the
    city host handed on is the earliest to have entered the city;
  - Change Gates' list (`LayerSiblingListHead/Data/Foot`): the world server's gates (from a city, its gate's
    cities), as MH3SP; our server closed the connection on it before. Picking one still fails (the jumps step);
  - the replay gate (`corpus::REACHED`) moves to M5; each scenario replays with the address it was recorded with
    (s12: the lab machine's new one).
- [ ] Recordings (D1) of the rest, each replaying whole (`recordings.md`).
- [x] Content (D2): checked against MH3SP's own generators (`tools/reference-content.py`, Python 2, and the
      `quests_match_the_reference_generator` test): all 40 quests of the live branch (`7e21e0a`) and of dev byte-identical.
      The only change needed: quests naming a small monster data file (`smallmonster_data_file`) instead of listing
      them; the live branch names six files it doesn't have, so those quests carry no small monsters (as served
      there). The other binaries' builders (server types, time events, NPC greeters, trading post) only changed in
      formatting; `arena_utils.py`'s additions are imported by nothing.
- [x] Announcements (D3): `POST /announce` on the health port, `mh3sp announce "<text>"`; every logged-in player,
      from "[MH3SP]", 30-character lines (tests: the cutting; two consoles at different places). To see on a console.
- [x] Stopping (D4): `stop.rs`, `POST /stop`, `mh3sp stop <delay|cancel>`; nothing refused meanwhile; a signal warns
      5 s, a second stops now (test: the whole sequence).
- [x] Metrics (D5): `metrics.rs`; `GET /metrics` (JSON), `GET /metrics/prometheus` (feature `prometheus`, written by
      hand: no dependency); the Dockerfile's `FEATURES` build argument for the `-prometheus` image.
- [x] Lab, run 15: an announcement shows in town chat (blue, "[MH3SP]", 30-character lines); a stop's warnings
      don't show in a quest; the server stopping mid-quest breaks the party (the leader sees the other leave at once,
      the other sees the leader frozen) and both end in a network error on the way back (11674, network instability).
- [x] Announcements in a hunt (runs 15–16): town chat doesn't show there; party chat (`NtcCircleChat`) only from a
      party member (`uniqueIdToMember`, the decomp): hunters get it as from themselves, named "[MH3SP]" (shown, in
      their party slot's colour), others town chat. `NtcCircleChat` relayed to the party (the game's fallback
      without a P2P session: s11 shows a hunt's chat is console to console). The departure refusal: dropped (D4).
- [x] Lab: the town interval (D7, run 17): 500 ms kept.
- [ ] Lab, another day: the mixed city on two machines (M4).
- [ ] The hour-long play session (exit criterion 4).
