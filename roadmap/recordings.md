# Console sessions to do

Every console session still pending, in order of use. Each scenario: what it's for, the setup, the steps (one
line per step, starting with the consoles concerned). Recordings against MH3SP (`tools/record-scenario.sh <name>
dev1 dev2`, AltWFC in full) go into `corpus/`; runs on the Rust server (AltWFC `nas` only) are checks. When a session
is done, it moves to "Done" with its date and what it showed.

The lab: `lab/play.sh` boots each console from its save state (the Village or City choice); `lab/cut.sh` and
`lab/p2p.sh` cut a console or the link between consoles; the user closes the consoles to end a run.

## Pending

### s13-seats (record against MH3SP, M5 D1)

City seats (1–12) and arm wrestling (13–14): `ReqLayerMediationLock/Unlock`. MH3SP lets two players share a seat and
never tells the others; the Rust server will fix both (check run below).

1. both: Online, slot 1, Valor1, City Gate 1. dev1: create City 1. dev2: join it.
2. dev1: sit on a seat, stand up, sit again. dev2: watch dev1 sit and stand.
3. dev2: try to sit on dev1's seat (MH3SP allows it).
4. both: arm wrestling together (the barrel).
5. dev1: sit, then quit the game while seated. dev2: is the seat free again?
6. both: close.

### s14-layer-tell (record against MH3SP, M5 D1)

Chat to one hunter (`ReqLayerTell`), which only exists in a gate or a world-server lobby (cities go console to
console), and only if the chat menu offers a recipient. If no menu offers it, `LayerTell` isn't built.

1. both: Online, slot 1, Valor1 (stay at the world server level).
2. both: open the chat: is there a way to pick one hunter as recipient? If so, dev1 sends dev2 `tell 1`.
3. both: City Gate 1; the same there (dev2 sends `tell 2`). Does the sender see its own line, once or twice?
4. both: close.

### Rust server checks (after building, M5)

- Seats on the Rust server: s13's steps; dev2 is refused dev1's seat; quitting while seated frees it.
- Player search on the Rust server: s12's steps 1–3 (dev1 is no longer among its own results, D9).

### The hour-long session (M5 exit criterion 4)

Two or three consoles on the Rust server (`hybrid`), an hour of normal play: cities, quests, chat, messages, friends,
seats, searches. Nothing should error or feel slower than MH3SP.

### A mixed city (M4, another day)

A Dolphin on a second machine (its own IP), on the Rust server in `hybrid`: one console forced to the relay (a failed
punch), the others direct; a city and a quest with all three. Shows a relayed and a direct pair side by side.

## Done

- 2026-10-09: runs 1–17 (M3, M4, M5): see the milestones' roadmaps.
- 2026-10-09, s12-player-search (against MH3SP): by name ('dev', both hunters; 'de'; 'zzz', nobody) and by Capcom ID
  all work; the searcher is among the results (grayed out by the console; we leave it out, M5 D9); "Warp to" fails
  (above, s15/s16). Its replay found three gaps, fixed (M5's work list): gates' live city lists, list order by
  arrival in the layer, Change Gates' list.
- 2026-10-09, s15-jumps (against MH3SP): Change Gates, the friends list's warp and the card's "Warp to" all fail the
  same way (the empty message, error `0x80060002`, no jump request): MH3SP's paths start with the depth where the
  console wants its version constant (M5 D11). The friends list warps through `ReqUserSearchInfo`, like the card. No
  menu found that searches cities. Built after it: the jumps (M5 D10–D13).
- 2026-10-09, jumps on the Rust server (M5 D10–D13): Change Gates to City Gate 2, the friends list's warp and the
  card's "Warp to" into dev2's city, and the friends list's warp to dev2 on another world server (Greed1: the
  reconnection and the ticket) all land where they should; no error or warning in the server's log. Not tried: a jump
  refused (a full or reserved city).
