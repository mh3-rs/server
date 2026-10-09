# M5 — Parity and polish

**Status: decisions settled (2026-10-09).**

Everything MH3SP does that we don't yet, the content and operations a public server needs, and the leftovers of
M1–M4. After M5 the Rust server can replace MH3SP for players (the website, M7–M8, and anti-cheat, M6, come after).

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
  how long is for moderation (M6–M8).
- **D7 Town position interval. Settled: a lab test, extremes included.** The layer record's field `0x11` is the
  console's position interval (how often it sends its own position; the decomp's `applyLoginRecord`), set by the
  server: 500 ms today (MH3SP's). Tried from one extreme to the other (e.g. 33 ms, one frame at 30 fps; 50, 100, 200,
  500, 1,000, 2,000 ms), noting for each how smooth town movement looks and the traffic measured (packets and bytes a
  second, per console and per full city): smooth but heavy, laggy but light, and the balance kept. Any console
  trouble at the low end ends that end.
- **D8 Tier 4 (live differential against MH3SP). Settled: dropped.** Kept for later twice; the corpus replays
  (tier 2) and console runs covered parity so far, and MH3SP is a moving reference. Revisit only if a divergence
  can't be found otherwise.

## Work (once the decisions are settled)

- [x] `roadmap/unk.md`: renames (no change on the wire); `NtcUserBinaryNotice` with `target` 1 to its one player
      (`profile_notices_reach_their_target`); the join/leave `state` note in the code.
- [ ] Recordings (D1) and the missing handlers, each replaying whole.
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
- [ ] Lab: the town interval (D7); the mixed city on two machines (M4).
- [ ] The hour-long play session (exit criterion 4).
