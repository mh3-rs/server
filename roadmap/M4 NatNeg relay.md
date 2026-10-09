# M4 — NAT negotiation and relay

**Status: draft (2026-10-09), decisions to settle.**

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

### Open (recommendation first)

- **D1 Scope.** NatNeg (UDP 27901) and the relay; **also the QR availability check** (UDP 27900, one fixed answer:
  a few lines), so AltWFC is left with NAS only (our own NAS stays a future step). Alternative: NatNeg only, QR stays
  with AltWFC.
- **D2 Strategies and default.** `NATNEG_STRATEGY = p2p | relay | hybrid` (environment variable), default
  **`hybrid`**: P2P first, the relay for a console known to fail. `relay` for players on a network known to block P2P
  (or a whole deployment), `p2p` for a LAN or a test.
- **D3 Hybrid: when to use the relay.** The choice can only be made **when sending the first `CONNECT`** (a second
  one is ignored, and nothing retries). So: (a) remember failures — a `REPORT` with failure, or the game's 11688
  in `NtcCollectionLog` — **per console** (by Capcom ID, in memory, 30 minutes renewed on each failure): its next
  negotiation, i.e. the player's retry, goes through the relay; (b) optionally, decide upfront from the `INIT`s
  (symmetric NAT spotted → relay at once), which needs natneg2/natneg3 on a second IP (D9). Injecting pings mid-attempt
  is possible but blind and blocked by restrictive NATs: no.
- **D4 Relay ports.** One UDP port **per pair** of consoles: each console sees its peer at `relay:port`, the relay
  forwards by source (from A to B, from B to A, nothing else). A console's datagrams for different peers leave one game
  socket, so one port per peer is the only way to tell them apart. Ports from a configured range
  (`RELAY_PORTS=40000-40999`: 1,000 pairs; a full city is 6 pairs), published by Docker as a range; freed when the pair
  is idle for 5 minutes (never sooner than the game's own 20 s) or both consoles leave the server.
- **D5 Who may use NatNeg and the relay.** Only consoles logged in to FMP: an `INIT`'s source IP must be the IP of a
  logged-in player (their FMP connection's); anything else is ignored. The relay forwards only between the two
  addresses the negotiation registered, so it's never an open relay. Per console: at most 8 relayed pairs, at most
  ~20 kB/s (100 × the measured rate). Over a limit: dropped, one `warn`.
- **D6 Addresses in the PAT connection offers. Answered (decomp):** GT2 uses NatNeg's address; the offers' IP:port
  only serve a same-LAN shortcut the game skips. No rewriting: NatNeg handing out relay addresses is enough. The
  relay learns each console's address from its first ping on the relay port (not from `INIT`: a symmetric NAT maps
  each destination differently), and forwards pings and game traffic through the same port.
- **D7 Topology. Answered (decomp):** a full mesh, pair by pair: up to 6 relayed pairs per city or party. To see with
  3 consoles (`lab/p3`, `p4` profiles exist), with one pair blocked (the session's own relay through a member).
- **D9 natneg1/2/3.** The game sends `INIT`s to three hostnames. One IP for all three works (what AltWFC does);
  a second IP for natneg2/natneg3 enables the upfront symmetric-NAT detection (D3 b). Recommendation: one IP now, the
  detection later if hybrid's "fail once, then relay" proves annoying.
- **D8 The relay's address.** `EXTERNAL_ADDRESS` (already the address consoles reach FMP at): one public IPv4. The
  relay and NatNeg listen on all interfaces, like the TCP roles.

## Work (once the decisions are settled)

- [x] Research (D6, D7): done, see "The game's NatNeg client".
- [ ] Lab: forced failure (pings dropped between the consoles): 11688 at ~6 s, no automatic retry, a new cookie
      when the player rejoins; a second `CONNECT` ignored.
- [ ] Wire: NatNeg records (magic, version, type, session id, port type, client index, then per type) typed and
      round-tripped on every recorded datagram; the QR availability packet (D1).
- [ ] NatNeg server (UDP 27901): `INIT`/`INIT_ACK`, `CONNECT` once both sides are in, `CONNECT_ACK`, `REPORT`/
      `REPORT_ACK`, sessions forgotten after a minute; only logged-in consoles (D5).
- [ ] Relay (D4, D5): ports from the range, forwarding by source, limits, idle cleanup.
- [ ] Strategies (D2, D3): `p2p`, `relay`, `hybrid` with the failure memory.
- [ ] Replay: the harness replays recorded UDP too (NatNeg exchanges; P2P datagrams aren't the server's).
- [ ] Tier 5: fake consoles through NatNeg and P2P in each strategy; simulated NAT; forced failure → relay on retry;
      relay latency (forwarding p99 under 1 ms on one host).
- [ ] Lab: the M3 run in each strategy with AltWFC's NatNeg off; a `relay` run with 3 consoles (D7); a 15 s cut
      through the relay (it must hold like P2P does).
- [ ] Deployment: the UDP ports in the Dockerfile and the README (27900, 27901, the relay range).
