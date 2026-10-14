# mh3sp

A server for *Monster Hunter Tri*'s online mode (Wii), in Rust: login, world servers, gates, cities, quests, chat,
friends, NAT negotiation and a relay for consoles that can't connect directly. Our server:
[mh3.pband.ch](https://mh3.pband.ch/patch). The community: [Discord](https://discord.gg/XvUNMytyM7).

## Playing

1. **What you need**: the European (`RMHP08`) or US (`RMHE08`) disc, and either Dolphin or a Wii that runs homebrew
   (Riivolution, or a USB loader).
2. **The patch**: [mh3.pband.ch/patch](https://mh3.pband.ch/patch) has the file for your disc and the way you play,
   and how to apply it. It points the game at our server and nothing else: no DNS to change, no hosts file, no game
   data.
3. **Play**: start the patched game and go online. The first time, the server gives your save an online support
   code: it is your account. Never share it nor your save; a lost one can't be recovered.
4. **Trouble**: the [known errors and error codes](https://mh3.pband.ch/errors), then the
   [Discord](https://discord.gg/XvUNMytyM7).

In Dolphin, play at normal speed: fast-forward (Tab) online makes the game drop itself (error 11674).

## Against MH3SP

[MH3SP](https://github.com/sepalani/MH3SP) (Python) is the reference. Each claim below names its proof; what isn't
proven isn't claimed.

**At parity** (tests, replays of recorded sessions, consoles): login, the world, cities, quests, chat, friends,
announcements; quest binaries byte-identical to MH3SP's generator (CI, through its binaries' hashes).

**Ahead:**
- **NAT negotiation and relay built in** (tests with simulated NATs, consoles). MH3SP needs AltWFC's NatNeg, which
  fails behind symmetric NATs and Docker (error 11688); we fall back to a relay, transparently.
- **Speed** (test `relay_latency`): a town position relayed under 10 ms at the 99th percentile, where MH3SP takes
  50–80 ms. Players who leave vanish at once (MH3SP: ~30 s).
- **Jumps work** (consoles): Change Gates, and "Warp to" from a found player or a friend (MH3SP's fail, M5 D11).
- **The hunter profile is validated** (tests), so a crafted one can't crash other consoles. Friend requests and
  messages are stored and delivered later (tests).
- **Administration** (tests; not yet seen on consoles): chat commands (`/tell` for everyone; `/announce`, `/stop`,
  `/kick`, `/ban` for admins), bans by console, an API with tokens, an admin CLI (`mh3sp-admin`) and an admin web
  page (`website`).
- **Operations** (tests, consoles): `mh3sp stop 5min` with in-game warnings, `mh3sp announce`, liveness and readiness
  probes, JSON and Prometheus metrics, anonymous logs, one SQLite file.

**Built, not yet seen on consoles:** seats held by one player, Player Search pages, Local Info's real population.
Profile ids stable across restarts (by construction: a hash of the Capcom ID).

**Behind:**
- **A whitelist.** MH3SP has an event whitelist; we have bans (by console), not a whitelist yet.
- **Languages.** MH3SP serves content to French, German, Italian and Spanish consoles; we serve English only.
- **Scaling out.** MH3SP can use MySQL and several processes; we run one process on SQLite, on purpose.

## Running your own server

The image is [`mh3rs/server`](https://hub.docker.com/r/mh3rs/server) on Docker Hub (amd64, arm64); the operator's
guide is its page there ([`deploy/README.md`](deploy/README.md) here).

## Development

`scripts/dev.sh` prepares `mh3sp.toml` and `.env` (sqlx's tools); `scripts/ci-tests.sh` runs what CI runs. Every quest of `CONTENT_DIR` is checked
byte-identical to MH3SP's own generator, through the hashes in `crates/server/src/content/reference.sha256`; a
content folder with quests it doesn't know adds them with `tools/reference-hashes.sh <checkout>` (Python 2 in
Docker). The working rules are in
[`CLAUDE.md`](CLAUDE.md).

## Sources

This server stands on others' reverse engineering:

- [MH3SP](https://github.com/sepalani/MH3SP) by sepalani and its contributors: the reference server (Python), its
  protocol knowledge and its event content.
- [mhtri-decomp](https://github.com/InusualZ/mhtri-decomp) by InusualZ: the game's matching decompilation (US), the
  ground truth for the console's behaviour.
- [MHTrIDA](https://github.com/sepalani/MHTrIDA) by sepalani: the packet table, error codes, save and quest formats,
  symbol maps.
- [MHTriServer](https://github.com/InusualZ/MHTriServer) by InusualZ: another server (C#), one class per packet.
- [dwc_network_server_emulator](https://github.com/polaris-/dwc_network_server_emulator) (AltWFC) by polaris- and
  its contributors: the Nintendo login, GameSpy's services and NAT negotiation.
- [MH3SP-patcher](https://github.com/sepalani/MH3SP-patcher) by sepalani: the game's patches.

## This repository

The [GitHub repository](https://github.com/mh3-rs/server) is a read-only mirror of `main`, pushed by our CI once
the checks and the image pass. Issues are off: talk to us on [Discord](https://discord.gg/XvUNMytyM7) or at
[pascal@pband.ch](mailto:pascal@pband.ch). The active plans are in [`roadmap/`](roadmap/).

## License

AGPL-3.0-or-later: [`LICENSE`](LICENSE).
