# Reachability check

**Status: agreed (2026-11-19), not started.**

The first player from outside (2026-11-18) got error 20100, then 11613. 20100 is the Nintendo login's request failing
or timing out without an HTTP answer (`DWCi/dwc_auth_interface.c:371`, `:648`): the user's network was down then.
11613 is a TCP error on the game's own connections (MHTrIDA: `getNetworkTCPErrorCode`, its cause undocumented): the
router forwarded port 80 but not 8200–8202, so OPN was refused, and the server logged nothing. The first deployment
(2026-10-11) had only been played from the LAN, which never needed the forwards. The server was right; nothing told
the operator. This adds a command an operator runs from another machine, which says which forward or record is
missing.

## Design

**`mh3sp check <domain>`**, from the image on any machine outside the server's network:
`docker run --rm mh3rs/server check <domain>` (the image's entrypoint is `mh3sp`). It reads no configuration (it
isn't the server) and takes no flags.

1. **The lookup.** The names of `patch::records()` (made `pub(crate)`, so the check and the patch can't drift),
   resolved to IPv4 only (the Wii speaks only IPv4). They must all agree: a name that doesn't resolve, or resolves
   elsewhere, fails naming its record. The comparison is a plain function of the resolved addresses.
2. **The probes**, each through the name the game uses for it, on the game's ports:

   | Name | Port | Passes when |
   |---|---|---|
   | `<domain>` | TCP 80 | `GET /patch` answers 200 |
   | `opn.<domain>` | TCP 8200 | a TCP connect succeeds |
   | `<domain>` | TCP 8201, 8202 | a TCP connect succeeds |
   | `mh3uswii.available.<domain>` | UDP 27900 | the QR availability answer comes back (`natneg::serve_qr`) |
   | `mh3uswii.a`, `.b`, `.c` `.<domain>` | UDP 27901 | NatNeg's `INIT` gets its `INIT_ACK` |

   Plain connects for 8200–8202: OPN's legacy TLS and LMP/FMP's framing have their own tests; what fails in practice
   is the forward. The NatNeg probe opens one negotiation, which expires like any other (32 per address allowed,
   `natneg.rs:30`): a run every minute stays far below.
3. **The relay ports are not checked**: a relay port is bound only while it relays a pair (`relay.rs:88`), so nothing
   answers on an idle one, and checking them would need a probe mode in the server. The output says so: "relay ports
   40000–40099 (UDP): not checked, forward them".
4. **Output**: one line per check, `ok` or `FAIL` with what to fix ("forward TCP 8200 to the server", "create the A
   record opn.<domain>"); 5 s timeout per check; exit 1 if any failed.

The ports are a parameter of the probing function, the game's defaults passed by the command: the tests run the
server on random ports (`tests/server.rs:58`), and port 80 needs root. An operator who changed `lmp_port` or
`fmp_port` isn't served yet (OPN, QR and NatNeg's ports are the game's own).

**The docs** (`deploy/README.md`, which already lists the ports and says Docker's published ports go around `ufw`):
"Ports to open" gets the check, run from another machine; "Alerting" gets the cron recipe for an outside machine,
`docker run --rm mh3rs/server check <domain> && curl -fsS <push URL>`, with an Uptime Kuma "Push" monitor (assumed:
not tried on the user's Kuma). The players' `/errors` page gets 11613 (seen: the game's TCP connection refused,
"tell the operator").

## Tests

- The lookup's comparison: all six the same passes; one elsewhere fails naming it; one missing fails naming it.
- A real lookup of a name under `.invalid` (never resolves, RFC 6761) fails. A successful lookup of real names isn't
  testable in CI: `*.localhost` doesn't resolve in `rust:1.98.0-bookworm` (tried, `getent ahostsv4 opn.localhost`:
  exit 2). The VPS run covers it.
- In `testkit`, against the real server on its random ports at `127.0.0.1`: every probe passes and the run exits 0.
- The same with one listener closed: the test first asserts that port refuses a connection, then expects `FAIL` on
  its line, the others `ok`, and exit 1.

## Not in this sprint

A relay probe; a TLS handshake with OPN; a self-check run by the server itself (its own router's loopback would give
false alarms); flags for changed LMP/FMP ports.

## Done when

- [ ] `mh3sp check` as designed, with its tests, CI green (`scripts/ci-tests.sh`).
- [ ] The docs: "Ports to open", "Alerting", the `/errors` page.
- [ ] [user] The router forwards TCP 8200–8202, UDP 27900–27901 and 40000–40099; from the VPS, `check mh3.pband.ch`
      shows every line `ok`.
