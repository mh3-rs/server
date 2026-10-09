# mh3sp

A server for *Monster Hunter Tri*'s online mode (Wii), in Rust: login, world servers, gates, cities, quests, chat,
friends, NAT negotiation and a relay for consoles that can't connect directly. The plan and every decision are in
[`roadmap/`](roadmap/).

## Running it

One process serves every role. Configuration is environment variables, each with a default (the full list:
[`crates/server/src/config.rs`](crates/server/src/config.rs)). The ones an operator sets:

| Variable | What |
|---|---|
| `EXTERNAL_ADDRESS` | The IPv4 address consoles reach this server at (handed out for LMP, FMP and the relay). Required. |
| `NATNEG_HOSTNAME` | The NAT negotiation hostname our disc patch writes into the game. Required. |
| `SECRET` or `SECRET__FILE` | The pepper of the support codes' HMAC. Required; never change it once consoles have registered. |
| `NATNEG_STRATEGY` | `hybrid` (default): direct, the relay for consoles that can't; `p2p` or `relay`: one way for everyone. |
| `RELAY_PORTS` | `40000-40999` (default): one UDP port per relayed pair of consoles. |
| `DATABASE_URL` | `sqlite://data/mh3sp.db` (default). |
| `CONTENT_DIR` | Event content (MH3SP's files), never in this repository. |

The Nintendo login (NAS) is still AltWFC's: run its NAS service next to this server (its NAT negotiation and QR
server must be off: this server holds their ports).

### Ports to open

Open them in the firewall and forward them from the router, unchanged (the server hands consoles the ports it listens
on):

| Port | Protocol | What |
|---|---|---|
| 8200 | TCP | OPN (TLS): the first connection |
| 8201 | TCP | LMP: login, Capcom IDs |
| 8202 | TCP | FMP: world servers, gates, cities, quests |
| 27900 | UDP | the game's availability check |
| 27901 | UDP | NAT negotiation |
| 40000–40999 | UDP | the relay (`RELAY_PORTS`) |
| 8280 | TCP | health (`/health/live`, `/health/ready`) and operator commands (`announce`, `stop`): keep it internal |

### DNS

The game looks up `natneg1`, `natneg2` and `natneg3` under the NAT negotiation domain: point all three at the
server. Optionally, point `natneg2` and `natneg3` at a **second** IP of the same server: NAT negotiation then spots
symmetric NATs and sends their pairs straight to the relay, instead of after one failed attempt (no setting needed).

### Announcements and stopping

These commands talk to the running server through its health port (8280), which must stay internal: anyone reaching
it could use them.

- `mh3sp announce "Maintenance tonight at 22:00"` sends a message to every logged-in player, shown in their chat from
  "[MH3SP]" (cut in lines of 30 characters).
- `mh3sp stop 15min` (or `1h30m`, `30s`…) stops the server with warning: players are told at once, reminders follow
  (1h, 30min, 15min, 5min, 1min, 30s, 10s, 5s before), then the server stops. Nothing is refused meanwhile: play goes
  on as usual until the end. Quests already under way are console to console and go on.
  `mh3sp stop cancel` changes your mind.
- `docker stop` (or Ctrl-C) gives players a 5-second warning; a second one stops at once. For a planned restart, use
  `mh3sp stop` first.

### Docker

NAT negotiation must see the consoles' real addresses (it hands each to its peer, and matches them with their logins),
so run the container with host networking:

```sh
docker run -d --name mh3sp --network host \
  -e EXTERNAL_ADDRESS=203.0.113.10 -e NATNEG_HOSTNAME=natneg1.example.net -e SECRET__FILE=/run/secrets/mh3sp \
  -v mh3sp-data:/opt/mh3sp/data mh3sp
```

Published ports (`-p`) would hide the consoles' addresses behind Docker's, and a thousand published UDP ports start a
thousand proxies.

## Development

`scripts/dev.sh` prepares `.env`; `scripts/ci-tests.sh` runs what CI runs. The working rules are in
[`CLAUDE.md`](CLAUDE.md).
