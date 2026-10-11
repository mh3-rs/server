# mh3sp

A server for *Monster Hunter Tri*'s online mode (Wii), in Rust: login, world servers, gates, cities, quests, chat,
friends, NAT negotiation and a relay for consoles that can't connect directly. The active plans are in
[`roadmap/`](roadmap/); the lasting decisions in the `docs` repository.

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

## Running it

An operator's guide. The server is one process for every role, configured by one TOML file, with one SQLite file. What it can't do yet, and what you need beside it, is said where it matters.

### What you need

- **A Linux host with a public IPv4 address**, and Docker with **host networking**: NAT negotiation must see the
  consoles' real addresses (it hands each to its peer, and matches them with their logins). Docker Desktop (macOS,
  Windows) and rootless Docker can't do host networking; nor can a published port (`-p`): it hides the consoles'
  addresses behind Docker's, and a thousand published UDP ports start a thousand proxies.
- **The event content**: a checkout of MH3SP's `event-quest-rotation-full` branch (its live server's quests, rotation
  and trading post), mounted read-only as `content_dir`. It is never in this repository nor in the image. At
  startup the server checks it and logs what is wrong (`content` lines): an `error` breaks a download, a `warn` is
  served as MH3SP serves it (six of the live branch's quests name small-monster files that don't exist: they spawn
  no small monsters, on MH3SP's live server too).
- **The Nintendo login (NAS)**: built in (port 80, plain HTTP), nothing to run beside the server. It also tells the
  server which Wii a console is on, so a ban holds on a new save.
- **The players' patch**: today MH3SP's ([MH3SP-patcher](https://github.com/sepalani/MH3SP-patcher)), which rewrites
  no hostname: players point the game's hostnames at your server themselves (DNS or a hosts file, below). A patch
  generated for your domain is planned.

### Quick start

[`deploy/compose.yaml`](deploy/compose.yaml) runs the server with everything mounted, from a checkout of this
repository:

```sh
cd deploy
git clone -b event-quest-rotation-full https://github.com/sepalani/MH3SP content
cp mh3sp.toml.example mh3sp.toml         # fill in external_address and natneg_hostname
openssl rand -hex 32 > secret && chmod 600 secret
mkdir data && sudo chown 999 data        # the image's user
docker login git.pband.ch                # until the image is on Docker Hub
docker compose up -d && docker compose logs -f
```

The image is `main`'s, from the project's Gitea registry for now (Docker Hub comes with the release). Everything you
add to `deploy/` stays out of git.

### Configuration

`mh3sp.toml` in the server's working directory (`/opt/mh3sp` in the image), or the file `MH3SP_CONFIG` names. Every
key has a default (the reference: [`crates/server/src/config.rs`](crates/server/src/config.rs)); an unknown key
stops the server, so a typo can't pass for a default.

| Key | What |
|---|---|
| `external_address` | The IPv4 address consoles reach this server at (handed out for LMP, FMP and the relay). Required. |
| `natneg_hostname` | The NAT negotiation hostname the players' patch writes into the game (today `natneg1.gs.nintendowifi.net`). Required. |
| `secret` or `secret__file` | The pepper of the support codes' HMAC (below); `secret__file` names a file that holds it, such as a Docker secret. Required. |
| `content_dir` | The event content (above). Default `content`. |
| `database_url` | `sqlite://data/mh3sp.db` (default). |
| `tls_cert`, `tls_key` | The OPN certificate (below). Default: the development one in `certs/dev`. |
| `nas_port` | The Nintendo login, plain HTTP. Default 80, the port the patched game calls. |
| `natneg_strategy` | `hybrid` (default): direct, the relay for consoles that can't; `p2p` or `relay`: one way for everyone. |
| `relay_ports` | `40000-40999` (default): one UDP port per relayed pair of consoles. |
| `opn_port`, `lmp_port`, `fmp_port`, `health_port` | 8200, 8201, 8202, 8280. Consoles are handed the LMP and FMP ports the server listens on: change them here, never in a port mapping. |
| `health_address` | `127.0.0.1` (default): where the health port listens (below). |
| `api_port`, `api_address` | 8281, `127.0.0.1` (default): the API (below). |
| `admins` | `[]` (default): the Capcom IDs allowed the admins' chat commands (below). |
| `encryption` | `false` (default); `true` turns on the game's packet encryption (an experiment). |

Logs: the `RUST_LOG` environment variable, `info,sqlx=warn` by default. They are anonymous: IDs, never names or
addresses; never a secret.

Without `external_address` or `natneg_hostname` the server runs but refuses every console ("not configured").

### The secret

`secret` is the pepper of the support codes: a console is recognised by an HMAC of its code with it, and the code
itself is never stored. **Losing or changing it loses every console**: none would be recognised, every player would
start over with new Capcom IDs. Make it once (`openssl rand -hex 32`), give it as a file (`secret__file`, a Docker
secret), and back it up with the database.

### TLS

Consoles open OPN over the Wii's legacy TLS, and the patched game trusts one CA: today MH3SP's, whose private key is
public, so `certs/dev` (a certificate from it) is what works, and what anyone could impersonate. A CA per deployment,
in a patch generated for it, is planned; until then `tls_cert` and `tls_key` stay on the default.

### Hostnames

Until the generated patch, every player's game must resolve these names to your server: through a DNS server you run
for them (a Wii's network settings take its address), or the hosts file of the machine running Dolphin:

- OPN: `mmh-t1-opn04.mmh-service.capcom.co.jp` (PAL; `opn03` on the US disc);
- NAT negotiation: `natneg1`, `natneg2`, `natneg3` under the `natneg_hostname` domain;
- the Nintendo login: `naswii.nintendowifi.net`;
- the availability check: `mh3uswii.available.gs.nintendowifi.net` (on PAL discs too).

The game reaches no other Nintendo or GameSpy service (`../docs/history/nintendo login.md`).

Optionally, point `natneg2` and `natneg3` at a **second** IP of the same server: NAT negotiation then spots
symmetric NATs and sends their pairs straight to the relay, instead of after one failed attempt (no setting needed).

### Ports to open

Open them in the firewall and forward them from the router, unchanged:

| Port | Protocol | What |
|---|---|---|
| 80 | TCP | the Nintendo login (HTTP): the first call, before OPN |
| 8200 | TCP | OPN (TLS): the first connection |
| 8201 | TCP | LMP: login, Capcom IDs |
| 8202 | TCP | FMP: world servers, gates, cities, quests |
| 27900 | UDP | the game's availability check |
| 27901 | UDP | NAT negotiation |
| 40000–40999 | UDP | the relay (`relay_ports`) |
| 8281 | TCP | the API: not to open as such, behind a reverse proxy with TLS |
| 8280 | TCP | not to open: health, metrics and operator commands, on 127.0.0.1 (`health_address`; with `0.0.0.0` for a metrics scraper elsewhere, keep it behind a firewall: its commands have no authentication yet) |

### Limits

Each console is held to what the game itself does, with room (the values are provisional): 16 connections per IP
address and 2000 in all; 5 new consoles an hour per address, and LMP only from an address that passed OPN; game
traffic relayed to others 20 messages a second, chat a line a second, quest joins one a second; 32 NAT negotiations
per address; a console that stops reading for 30 s is dropped. Loopback is exempt from the per-address limits.
`GET /metrics` counts what they refused (`limited`): if it grows with ordinary play, a limit is too tight (players
behind one carrier-grade NAT share an address).

### Announcements and stopping

These commands talk to the running server through its health port (8280), on 127.0.0.1 by default: anyone reaching
it could use them (no authentication yet).

- `mh3sp announce "Maintenance tonight at 22:00"` sends a message to every logged-in player, shown in their chat from
  "[MH3SP]" (cut in lines of 30 characters).
- `mh3sp stop 15min` (or `1h30m`, `30s`…) stops the server with warning: players are told at once, reminders follow
  (1h, 30min, 15min, 5min, 1min, 30s, 10s, 5s before, shown in hunts too), then the server stops. Nothing is refused
  meanwhile; but a hunt doesn't survive the server stopping (its players end in a network error on their way back to
  town), so give players time.
  `mh3sp stop cancel` changes your mind.
- **In the game's chat**, a line starting with `/` goes to the server, never to other players, and its answer to
  the sender alone, from "[MH3SP]". Anyone can `/tell <player> <message>` (a Capcom ID, or a name only one player
  online has; not delivered to a player who has the sender on their black list). The Capcom IDs listed in `admins` also have `/announce <text>`, `/stop <delay|cancel>`,
  `/kick <player>`, `/ban <Capcom ID> <reason>` (the player's console, all its hunters; kicked at once, and refused
  at every reconnection; a player can still come back as a new console, with a save that has no support code yet),
  `/unban <Capcom ID>`, and with the quest bots `/bot add` (in their party) and `/bot remove <bot>`; to anyone else,
  these are an unknown command.
- `docker stop` (or Ctrl-C) gives players a 5-second warning; a second one stops at once. For a planned restart, use
  `mh3sp stop` first.

### The API

The admin CLI and the website talk to the server through its API (JSON on `api_port`, 127.0.0.1:8281 by default;
put a reverse proxy with TLS in front of it to reach it from elsewhere). Each request carries a token. Make the
first user, and their token, with the binary itself (it opens the database directly):

```sh
docker compose exec mh3sp mh3sp user add you@example.org admin   # prints a token, once
docker compose exec mh3sp mh3sp token add you@example.org        # another one
docker compose exec mh3sp mh3sp token revoke mh3sp_0123abcd…     # the part before the dot
docker compose exec -T mh3sp mh3sp user password you@example.org < password.txt   # the website's sign-in
```

Admins have `GET /api/players` and `/api/bans`, and `POST /api/announce`, `/api/stop`, `/api/kick`, `/api/tell`,
`/api/ban`, `/api/unban`, with the quest bots `/api/bot/add` and `/api/bot/remove` (the messages: the `shared`
crate; the admin CLI, `mh3sp-admin`, calls them). Anyone with a token has `GET /api/me`; the website signs in with
`POST /api/session` (email and password) and out with `POST /api/logout`.

### Metrics

`GET /metrics` on the health port: players (per world server), cities, quests, NAT negotiations (direct, relayed,
reports), the relay (pairs open, bytes), requests (count, latency buckets), as JSON. Counts only, never a player's
identity. The `-prometheus` images also serve `GET /metrics/prometheus`, the same in Prometheus' format (built with
`--build-arg FEATURES=prometheus`, or `cargo build --features prometheus`).

### Maintenance mode

`mh3sp maintenance "Back at 18:00"` runs OPN alone, without the database: every console is refused with the message
(the game's own refusal screen). For work on the database or the host; stop the server first (`mh3sp stop`).

### Backups and upgrades

Back up `data/mh3sp.db` **and the secret**: one without the other restores nothing. The database is SQLite in WAL
mode; copy it live with `sqlite3 data/mh3sp.db ".backup backup.db"`, not with `cp`.

Before 1.0.0 the schema is rewritten in place: an upgrade may need a fresh database (the release notes say so). From
1.0.0 on, migrations only go forward and run at startup.

### Sizing

One process holds hundreds of players: what costs is the relay, about 0.25 kB/s each way per relayed pair of
consoles (a four-player quest is six pairs), and a TLS handshake (RSA) per login. Hunts are console to console; the
server only relays the pairs that can't reach each other.

### Alerting

`/health/live` restarts a dead server (the image's `HEALTHCHECK`); `/health/ready` says whether the database answers.
The one failure players can't work around is NAT negotiation: nobody can join a city (MH3SP's longest outage, eight
hours). With the `-prometheus` image, alert when negotiations stop pairing while players are on:

```yaml
- alert: NatNegNotPairing
  expr: mh3sp_players > 4 and increase(mh3sp_natneg_pairs_total[30m]) == 0
  for: 30m
```

## Development

`scripts/dev.sh` prepares `mh3sp.toml` and `.env` (sqlx's tools); `scripts/ci-tests.sh` runs what CI runs. Every quest of `CONTENT_DIR` is checked
byte-identical to MH3SP's own generator, through the hashes in `crates/server/src/content/reference.sha256`; a
content folder with quests it doesn't know adds them with `tools/reference-hashes.sh <checkout>` (Python 2 in
Docker). The working rules are in
[`CLAUDE.md`](CLAUDE.md).

## License

AGPL-3.0-or-later: [`LICENSE`](LICENSE).
