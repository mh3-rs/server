# Console sessions to do

Every console session still pending, in order of use. Each scenario: what it's for, the setup, the steps (one
line per step, starting with the consoles concerned). Recordings against MH3SP (`tools/record-scenario.sh <name>
dev1 dev2`, AltWFC in full) go into `corpus/`; runs on the Rust server (AltWFC `nas` only) are checks. When a session
is done, it moves to "Done" with its date and what it showed.

The lab: `lab/play.sh` boots each console from its save state (the Village or City choice); `lab/cut.sh` and
`lab/p2p.sh` cut a console or the link between consoles; the user closes the consoles to end a run.

## Pending

### Rust server checks (after building, M5)

- Seats on the Rust server: s13's steps; dev2 is refused dev1's seat; quitting while seated frees it.
- Player search on the Rust server: s12's steps 1–3 (dev1 is no longer among its own results, D9).

### The hour-long session (M5 exit criterion 4)

Two or three consoles on the Rust server (`hybrid`), an hour of normal play: cities, quests, chat, messages, friends,
seats, searches. Nothing should error or feel slower than MH3SP.

### A mixed city (M4, another day)

A Dolphin on a second machine (its own IP), on the Rust server in `hybrid`: one console forced to the relay (a failed
punch), the others direct; a city and a quest with all three. Shows a relayed and a direct pair side by side.

### Voice chat through the relay (later)

The game's Wii Speak voice goes console to console, in the P2P session (the decomp: `NetworkLayerPat`'s voice
buffers, `NetworkWiiMediator`'s microphone and encoder), so through our relay for relayed pairs. The relay caps each
console at 20 kB/s per pair (M4 D5, `relay.rs`), sized on game traffic measured without voice (~0.23 kB/s): voice must
fit under it, or it gets cut silently.

Setup: the Rust server in `relay` (every pair relayed); Dolphin's emulated Wii Speak on both consoles (the Mac's
microphone; recent Dolphin versions).
1. both: Online, slot 1, Valor1, City Gate 1. dev1: create City 1. dev2: join it.
2. both: talk for a minute (speak into the Mac's microphone): does each hear the other?
3. both: a quest together, talk again during the hunt.
4. both: close.

Then measure the relayed rate per console (the captures, or the relay's metrics) against the cap; no `warn`
"over its rate" in the log.

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
- 2026-10-09, s13-seats (against MH3SP): sitting and standing show on the other console; MH3SP lets dev2 sit on
  dev1's seat while dev1 is on it and tells nobody; arm wrestling works; dev1 closing its window while seated leaves
  the seat free (MH3SP never held it). Replays on the Rust server, the refused seat declared (`[[omit]]` with
  `alert`: an answer replaced by a refusal, new in the harness).
- 2026-10-10, s14-layer-tell (against MH3SP): no chat to one hunter anywhere (the world server level has no chat; a
  gate's is everyone's), friends or not; the friends list's message is an inbox one. `LayerTell` not built (M5 D14).
