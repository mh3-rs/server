# mh3sp-rs

A Rust rewrite of the Monster Hunter Tri (Wii) server. Plans and decisions: `roadmap/` (`00. global.md` first: status and order; `architecture.md`: the lasting decisions).

## Sources

When a packet layout or a game behaviour is in doubt, check the reverse-engineering sources before guessing.

| Source | Use |
|---|---|
| [MH3SP](https://github.com/sepalani/MH3SP) (`../reference`, branch `dev`, corpus recorded at `f53b06c`) | The reference server (Python): `mh/pat.py`, `fmp_server.py`, `mh/pat_item.py`, `mh/constants.py`, `mh/database.py`, `mh/quest_utils.py`, `mh/time_utils.py`; event content in `event/` (`CONTENT_DIR` in tests and CI). The lab's server reads the live branch's content instead: `../content`, a worktree of `event-quest-rotation-full` at `7e21e0a` (M5 D2) |
| [mhtri-decomp](https://github.com/InusualZ/mhtri-decomp) (`../lab/mhtri-decomp`) | Matching decompilation of the game (US, `RMHE08`): ground truth for client behaviour (`src/Network`, `src/DWCi/DWCi_NatNeg.c`) |
| [MHTrIDA](https://github.com/sepalani/MHTrIDA) (`../lab/mhtrida`) | Packet table with Japanese names (`server/doc/PAT.MD`), error codes (`server/ERROR.md`), save format, quest files, PAL/US/JP symbol maps, Wireshark dissector |
| [MHTriServer](https://github.com/InusualZ/MHTriServer) (`../lab/mhtriserver`) | Another server (C#): one typed class per packet |
| [dwc_network_server_emulator](https://github.com/polaris-/dwc_network_server_emulator) (AltWFC, `../lab/altwfc`) | Nintendo login and GameSpy services; NAT negotiation protocol (`gamespy_natneg_server.py`) |
| [MH3SP-patcher](https://github.com/sepalani/MH3SP-patcher) (`../lab/patcher`) | The game patches (Riivolution) the lab uses |

The quest bots' brain is its own repository, `../brain` (`mh3sp-brain`); the messages it and the server exchange are
`../tunnel`, pinned by commit in both (`crates/server/Cargo.toml`). The lab (`../lab/`, not a repository): `altwfc.sh`, `play.sh <rvz> [profile] [mac]`, `hosts.sh on|off` (sudo,
the user runs it), `server.sh` (Python MH3SP); `tools/oracle.sh` restarts Python MH3SP on a fresh database.

Lab traps: run Dolphin at normal speed (never Tab online: the game drops itself after 90 s of its own time without
a server line check, error 11674); Dolphin writes its capture (`<profile>/Dump/SSL/*.pcap`) only when it closes;
restarting the server disconnects the consoles. Bots in the lab: the server built with `--features bot` and run with `BOT_PORT`, `mh3sp-brain` (from
`../brain`) reading its commands on stdin (`follow <bot> <hunter>`, `stay <bot>`, `move`, `area`), `mh3sp bot add
<player>`. When a behaviour looks wrong in the lab, read the capture before changing the design.

## Rules

- No content or game data in this repository or the image (MH3SP's event files included): it lives in
  `CONTENT_DIR`.
- Logs are anonymous (IDs only, no names, no IP addresses); secrets (support codes, PAT tickets, `SECRET`) are
  never logged.
- A feature deferred hides behind a function with its final signature (architecture decision 10).
- Byte work with the `bytes` (`Buf`/`BufMut`) and `hex` crates, never hand-rolled.
- `Option<T>` rather than an `Unknown` enum variant.
- No internal primary keys in users' hands (UUID public ids); support codes stored only as HMAC-SHA256 with `SECRET`.
- The schema is `migrations/0001_schema.sql`; `scripts/dev.sh --reset` after changing it. After changing a query
  or the schema, `scripts/sqlx-prepare.sh`.
- The CI script is `scripts/ci-tests.sh` (not `scripts/ci.sh`).

## Not yet on the global workflow

Until this repo is migrated, these override the global CLAUDE.md:

- One branch, `main`: no `dev`/`prod`.
- Settled decisions go into the roadmap (the milestone's file, lasting ones in `architecture.md`), not `docs/`;
  pending console sessions into `roadmap/recordings.md`.
