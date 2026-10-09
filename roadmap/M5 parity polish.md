# M5 — Parity and polish

**Status: draft (2026-10-09), decisions to settle.**

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

### Open (recommendation first)

- **D1 New recordings.** Like M3: two (or three) Dolphins against MH3SP, one scenario per missing feature: party chat
  (`s11`), player and city searches (`s12`), whatever brings sibling lists, mediation lock and `LayerTell` (to find:
  the decomp says when the console sends them). A feature the game never sends from its menus isn't implemented.
- **D2 Content: the live branch's files.** `CONTENT_DIR` pointed at a checkout of MH3SP's `event-quest-rotation-full`
  (content stays out of this repository); our quest generator updated to its rework (no `.dat` files), checked
  byte-identical on all its quests. Live rotation updates: already ours (files read on demand).
- **D3 Announcements.** As MH3SP sends them (city chat from "[MH3SP]", 30-character lines), to every logged-in player
  (wherever they are: world server, gate, city; in a quest too). Triggered by the operator: `mh3sp announce "text"`,
  which talks to the running server through its health port (a `POST /announce`, internal like `/health`). The
  website (M8) uses the same endpoint. The same path serves "system" messages later (e.g. the friend-removed idea).
- **D4 Graceful shutdown.** On SIGTERM (Docker's stop): an announcement ("server restarting in 1 minute"), then a
  minute later maintenance mode (new logins refused with the maintenance text), then the connections closed. The
  delay configurable (`SHUTDOWN_GRACE`, default 60 s; 0 for tests). Quests in progress are P2P and go on until the
  players return to town.
- **D5 Metrics.** A Prometheus endpoint on the health port (`/metrics`): players online (per world server), cities,
  quests, NatNeg negotiations (direct, relayed, failed), relay pairs and traffic, request latency. No database, no
  history (Prometheus keeps it). Anonymous only: counts, never Capcom IDs.
- **D6 Chat logs. Recommendation: no.** MH3SP's branch logs chat; ours never logs what players write (user content,
  M1's logging rule). Moderation (M6–M8) will decide what to keep, where, how long.
- **D7 Town position interval.** A lab test with 200 ms, then 100 ms (field `0x11`): smoother? Any console trouble?
  Then pick the lowest that works (traffic: ~80 bytes per position, ~2 kB/s per full city at 100 ms).
- **D8 Tier 4 (live differential against MH3SP). Recommendation: drop it.** Kept for later twice; the corpus replays
  (tier 2) and console runs covered parity so far, and MH3SP is a moving reference. Revisit only if a divergence
  can't be found otherwise.

## Work (once the decisions are settled)

- [ ] `roadmap/unk.md`: renames; `NtcUserBinaryNotice` routing (a test with `target` 1).
- [ ] Recordings (D1) and the missing handlers, each replaying whole.
- [ ] Content (D2): the generator's rework, the live branch's quests byte-identical.
- [ ] Announcements (D3): the endpoint, `mh3sp announce`, a test.
- [ ] Graceful shutdown (D4).
- [ ] Metrics (D5).
- [ ] Lab: the town interval (D7); a quest ending without the server (M3 D4); the mixed city on two machines (M4).
- [ ] The hour-long play session (exit criterion 4).
