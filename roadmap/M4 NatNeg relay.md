# M4 — NAT negotiation and relay

**Status: done (2026-10-09), but the mixed-city run with a second machine (kept for another day).**

The consoles play towns and hunts peer to peer (UDP, GameSpy GT2); the server only matchmakes. To connect, two
consoles go through GameSpy's **NAT negotiation** (NatNeg, UDP 27901), done today by AltWFC. M4 brings it into the
server, and adds a **relay** for players whose NAT can't be punched (strict NAT, CGNAT, some mobile and university
networks): the negotiation hands each console the relay's address instead of its peer's, and the relay forwards their
datagrams. No game patch needed: the game always goes through NatNeg.

## Exit criteria

1. The M3 run (two consoles in one city and one quest) passes in each strategy (`p2p`, `relay`, `hybrid`) with
   AltWFC's NAT negotiation turned off.
2. Tier 5 green: fake peers through NAT negotiation in each strategy, simulated NAT (address and port rewriting),
   a forced P2P failure falling back to the relay.
3. The recorded NatNeg exchanges replay against the server (tier 2, UDP).

## What the game uses (lab, 2026-10-09)

Of AltWFC's services the game uses three, never GameSpy's friends or profiles (GPCM/GPSP):

| Service | Port | What the game does |
|---|---|---|
| NAS (Nintendo login, HTTP) | 80/443 | at each login: `acctcreate`, `login`, `svcloc` |
| QR (master server) | UDP 27900 | one availability check per login: "is `mh3uswii` online?" (`09 00000000 "mh3uswii"` → `fe fd 09 00000000`) |
| NatNeg | UDP 27901 | once per pair of consoles that connect (town and quest) |

## Protocol: what the recordings show

Every scenario's `udp/` files hold the consoles' datagrams (NatNeg and P2P). Per negotiation, each console:

1. sends **`INIT`** (`0x00`) three times, from two sockets: port type 0 from a helper socket, 1 and 2 from its
   **game socket** (the one P2P traffic uses), each with the session id (a cookie both consoles got from the server:
   `NtcLayerBinary2` / `NtcCircleBinary2`), its client index (0 client, 1 host), its local address and the game name;
   the server answers each with **`INIT_ACK`** (`0x01`);
2. receives **`CONNECT`** (`0x05`) from the server once both sides of the session have sent `INIT`: the peer's address
   (IP and port) to talk to; answers **`CONNECT_ACK`** (`0x06`);
3. exchanges **`CONNECT_PING`** (`0x07`) directly with the peer (the hole punch);
4. sends **`REPORT`** (`0x0D`): success or failure, its NAT type and mapping scheme; the server answers
   **`REPORT_ACK`** (`0x0E`).

Counts over the corpus: 68 `INIT`, 64 `CONNECT`, 32 `CONNECT_PING` each way, 16 `REPORT`, then ~12,000 P2P datagrams
each way (towns and hunts). Measured P2P traffic: ~5.5 packets/s, ~42 bytes each, ~0.23 kB/s per direction.

### The game's NatNeg client (decomp, 2026-10-09)

Read in `mhtri-decomp/src/DWCi/DWCi_NatNeg.c` (DWC's build of GameSpy's `natneg.c`) and the GT2 glue
(`GameSpyInterfaceThread.cpp`, `NetworkLayerPat.cpp`, `NetworkSessionStable.cpp`), checked against the corpus.

- **Cookie** = the XOR of the two consoles' PAT peer ids (relayed by the server in `NtcLayerBinary(2)`): the server
  can tell which pair a negotiation is for.
- **INIT**: port type 0 from the game socket to natneg1; 1 from a NatNeg socket to natneg1; 2 and 3 from that socket
  (carrying the game socket's local port) to **natneg2 and natneg3**. Resent every 500 ms, 10 times; then error 11687.
  Once all are acked, the client waits **60 s** for `CONNECT` (else 11686).
- **CONNECT** is acked whenever it comes, but **acted on only before pinging starts**: a second one, e.g. a relay
  address after a failed punch, is ignored. No check of who sent it.
- **Pings**: from the game socket to the given address, every 700 ms, 7 retries: a failed punch gives up after
  ~6.3 s, sends `REPORT` with failure, and shows **11688**. A ping's *source* becomes the peer address (no check).
- **Nothing retries a failed negotiation**; a pair whose established link drops is renegotiated with the same cookie.
  A player rejoining gets a new cookie. (`0x80030042`, 11677, is not a NatNeg error: a peer link lost.)
- **GT2 uses NatNeg's address**, not the IP:port in the PAT offers (those are only for a same-LAN path, skipped
  since MH3SP's "mode" values differ per user). **GT2 checks the source address** of every datagram: the address
  that answers the pings must be the one that carries the game traffic.
- **No NAT-type detection** in the client (never sends `ADDRESS_CHECK` or `NATIFY`; `REPORT` always says "unknown").
  The server can still classify: different public ports for port types 1 and 2/3 (sent to natneg1 vs natneg2/3 on
  *different IPs*) mean a symmetric NAT.
- **Topology: a full mesh**, negotiated pair by pair (the host with each member, then the members two by two): 6 pairs
  for 4 players. The session also has its own relay through another member (to explore: it may tolerate one failed
  pair with 3+ players).
- Keepalives: every 1 s; a peer silent for 20 s is dropped (matches the M3 D4 cut test).

From the disconnect test (M3 D4): the P2P link survives 15 s of silence, not 20 (both see "dev exited the game"). The
relay must never be the reason a link drops: no idle timeout under a few minutes, no queueing.

## Decisions

Settled with the user on 2026-10-09.

- **D1 Scope. Settled:** NatNeg (UDP 27901), the relay, and the QR availability check (UDP 27900, one fixed answer),
  all in the server: one project, one process. AltWFC keeps NAS only (our own NAS stays a future step).
- **D2 Strategy. Settled:** `NATNEG_STRATEGY = hybrid | p2p | relay` (configuration), default `hybrid`. `p2p` and
  `relay` force one way for everyone.
- **D3 Hybrid. Settled: transparent when possible, else on the first failure.** Each player carries an
  `Option<Nat>`, `Nat` being `Direct | Relay` (no "hybrid" at that level; `None`: not known yet), learned from its
  negotiations; it's a property of the NAT, so it's set on every logged-in player at that IP. `p2p` and `relay` don't
  use it. A pair is relayed if either console is `Relay`:
  - *at once*, for a **symmetric NAT**: the console's port type 1 `INIT` (to natneg1) and 2 (to natneg2, same socket)
    arrive from different ports. Needs natneg2/natneg3 to resolve to another IP than natneg1 (DNS only: with one IP
    the ports always match, so the check never misfires and needs no setting);
  - *after a failure*: a direct pair's `REPORT` with failure flags both IPs `Relay` (which side can't punch is
    unknown): the player's retry (rejoining the city or quest) goes through the relay. A second `CONNECT` is ignored
    by the game and nothing retries by itself (lab run 9). A success sets `Direct` (never undoes `Relay`).
  The filtering probe (`ERT_TEST`) was dropped: it can only show that a NAT is open, and an open NAT never needs the
  relay.
- **D4 Relay ports. Settled:** one UDP port per pair (forwarding by source: from A to B, from B to A, nothing else),
  from a configured range (`RELAY_PORTS`, e.g. `40000-40999`: 1,000 pairs; a full city or party is 6), freed after
  5 idle minutes (never under the game's 20 s) or when both consoles have left. **The operator opens UDP 27900, 27901
  and the relay range** (firewall, router, Docker): in the README.
- **D5 Abuse. Settled:** every check we can apply: only consoles logged in to FMP negotiate (an `INIT`'s source IP
  must be a logged-in player's); the relay forwards only between the two addresses its pair negotiated (never an
  open relay); per console at most 8 relayed pairs and ~20 kB/s (100 × the measured rate); over a limit: dropped, one
  `warn`.
- **D6 Addresses in the PAT offers. Answered by the decomp, to confirm in the lab:** GT2 uses NatNeg's address; the
  offers' IP:port only serve a same-LAN shortcut the game skips. No rewriting. The relay learns each console's address
  from its first ping on the relay port (not from `INIT`), and carries pings and game traffic through the same port
  (GT2 checks the source address).
- **D7 Topology. Answered by the decomp, to confirm in the lab:** a full mesh negotiated pair by pair (up to 6 pairs);
  with 3 consoles, one pair blocked (the session's own relay through a member).
- **D8 Relay address. Settled:** in the configuration: `EXTERNAL_ADDRESS` (the address consoles reach FMP at).
- **D9 A second address for NAT detection. Settled: optional, DNS only** (deployments rarely have a second IP):
  pointing natneg2/natneg3 at a second IP of the server turns the symmetric-NAT check on (D3); no setting.

## Work (once the decisions are settled)

- [x] Research (D6, D7): done, see "The game's NatNeg client".
- [x] Lab, forced failure (run 9, 2026-10-09; `lab/p2p.sh block` drops console-to-console UDP, NatNeg through):
      the joining console pings 9 times 0.7 s apart, sends `REPORT` with failure at +6.5 s and shows 11688; the city
      host sees "entered" then "left"; no new `INIT` by itself. Rejoining: a new negotiation, a new cookie, the same
      failure. Unblocked: pings answered in 50 ms, `REPORT` success. AltWFC sends one `CONNECT` per `INIT`: the extra
      ones, arriving while pinging, are acked and ignored (the second-`CONNECT` rule, seen live).
- [x] Wire: NatNeg records (magic, version, type, session id, port type, client index, then per type) typed and
      round-tripped on every recorded datagram; the QR availability packet (D1).
- [x] NatNeg server (UDP 27901): `INIT`/`INIT_ACK`, `CONNECT` once both sides are in, `CONNECT_ACK`, `REPORT`/
      `REPORT_ACK`, sessions forgotten after a minute; only logged-in consoles (D5).
- [x] Relay (D4, D5): ports from the range, forwarding by source, limits, idle cleanup.
- [x] Strategies (D2, D3): `Option<Nat>` per player (by IP); the symmetric-NAT check; a pair relayed if either is
      `Relay`; the failure fallback.
- [x] Replay (`natneg_exchanges_replay`): every scenario's NatNeg and QR datagrams, all consoles merged in time order,
      against the server: the same answers per socket; `CONNECT`s compared per console (which socket gets one
      differs: the game takes either) with the same peer game sockets; AltWFC's `CONNECT`s carrying a peer's NatNeg
      socket port (a quirk, ignored by the game) not expected. Found on the way: like AltWFC, a pair's `CONNECT`
      now goes to every socket a console used (a lost one is never asked for again).
- [ ] Tier 5: fake consoles through NatNeg and P2P in each strategy; simulated NAT; forced failure → relay on retry;
      relay latency (forwarding p99 under 1 ms on one host).
- [x] Lab, `relay` (run 10, 2026-10-09; AltWFC on NAS only, `lab/altwfc.sh nas`): a city and a quest together, all
      through one relay port (the quest reuses the city's link: no new negotiation); D6 confirmed: no rewriting.
- [x] Lab, `p2p` (run 11): the same, direct, through our NatNeg. Timings as with AltWFC: entering a city takes ~5–6 s,
      almost all the game's own wait before `REPORT` (5 s after the peer answers); our `CONNECT` comes 5 ms after
      the `INIT`s (AltWFC ~70 ms). Joining a party: no negotiation, answered in ~30 ms.
- [x] Lab, `hybrid` (run 12): direct link blocked (`lab/p2p.sh block`, relay range let through): the first join fails
      (both report failure, 11688), the retry opens a relay, then a city and a quest through it, still blocked.
      Negotiations happen at the city level only (each pair of city members); a party reuses the city's links.
- [x] Lab, a 15 s cut through the relay (run 13, `relay`): in town and mid-quest, the cut console freezes for the
      other, then comes back; same connections, same relay pair, no new negotiation: as with a direct link (M3 D4).
- [x] Lab, 3 consoles in `relay` (run 14): a city and a quest together; three relay pairs (the full mesh, D7). The
      game negotiates them one after another, ~5 s each (its own wait before `REPORT`): a console entering a city
      with two players in it is fully linked 12.7 s later. Nothing to win on our side (our part takes milliseconds).
- [ ] Lab, another day: a mixed city (one console relayed, the others direct: decided per pair, unit-tested), with a
      Dolphin on another machine (the lab's consoles share one IP, so one flag relays them all).
- [x] Deployment: `README.md` (the settings an operator sets, the ports to open, natneg1/2/3's DNS, Docker with
      host networking: NAT negotiation needs the consoles' real addresses); the Dockerfile exposes the UDP ports.
