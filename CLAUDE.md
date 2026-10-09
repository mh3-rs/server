# mh3sp-rs

A Rust rewrite of the Monster Hunter Tri (Wii) server. Plans and decisions: `roadmap/` (`00. global.md` first: status and order; `architecture.md`: the lasting decisions).

## Sources

When a packet layout or a game behaviour is in doubt, check the reverse-engineering sources before guessing.

| Source | Use |
|---|---|
| [MH3SP](https://github.com/sepalani/MH3SP) (`../mh3sp`, branch `dev`, corpus recorded at `f53b06c`) | The reference server (Python): `mh/pat.py`, `fmp_server.py`, `mh/pat_item.py`, `mh/constants.py`, `mh/database.py`, `mh/quest_utils.py`, `mh/time_utils.py`; event content in `event/` (`CONTENT_DIR` in dev and tests) |
| [mhtri-decomp](https://github.com/InusualZ/mhtri-decomp) | Matching decompilation of the game (US, `RMHE08`): ground truth for client behaviour (`src/Network`, `src/DWCi/DWCi_NatNeg.c`) |
| [MHTrIDA](https://github.com/sepalani/MHTrIDA) (`../mh3sp/lab/mhtrida`) | Packet table with Japanese names (`server/doc/PAT.MD`), error codes (`server/ERROR.md`), save format, quest files, PAL/US/JP symbol maps, Wireshark dissector |
| [MHTriServer](https://github.com/InusualZ/MHTriServer) (`../mh3sp/lab/mhtriserver`) | Another server (C#): one typed class per packet |
| [dwc_network_server_emulator](https://github.com/polaris-/dwc_network_server_emulator) (AltWFC, `../mh3sp/lab/altwfc`) | Nintendo login and GameSpy services; NAT negotiation protocol (`gamespy_natneg_server.py`) |
| [MH3SP-patcher](https://github.com/sepalani/MH3SP-patcher) (`../mh3sp/lab/patcher`) | The game patches (Riivolution) the lab uses |

The lab (`../mh3sp/lab/`, not a repository): `altwfc.sh`, `play.sh <rvz> [profile] [mac]`, `hosts.sh on|off` (sudo,
the user runs it), `server.sh` (Python MH3SP); `tools/oracle.sh` restarts Python MH3SP on a fresh database.

## Rules

- No content or game data in this repository or the image (MH3SP's event files included): it lives in
  `CONTENT_DIR`.
- Logs are anonymous (IDs only, no names, no IP addresses); secrets (support codes, PAT tickets, `SECRET`) are
  never logged.
- YAGNI, and code written once: no stand-ins to replace later, no single-implementation traits. A feature deferred
  hides behind a function with its final signature (architecture decision 10).
- Byte work with the `bytes` (`Buf`/`BufMut`) and `hex` crates, never hand-rolled.
- `Option<T>` rather than an `Unknown` enum variant.
- Before 1.0.0, one schema file (`migrations/0001_schema.sql`) rewritten in place; `scripts/dev.sh --reset` after
  changing it.
- No internal primary keys in users' hands (UUID public ids); support codes stored only as HMAC-SHA256 with `SECRET`.
- `scripts/ci-tests.sh` must pass before a commit, chained with `&&`, never `;`:
  `scripts/ci-tests.sh > /tmp/ci.log 2>&1 && git add -A && git commit …`. After changing a query or the schema,
  `scripts/sqlx-prepare.sh`.
- Commits end with the `Co-Authored-By` and `Claude-Session` lines; the user pushes.
- Settled decisions go into the roadmap (the milestone's file, lasting ones in `architecture.md`); pending console
  sessions into `roadmap/recordings.md`.
